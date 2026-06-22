//! Auto session prime — send a minimal "hi" to a subscription account so a fresh 5-hour
//! window opens, anchoring the reset clock to the user's work rhythm.
//!
//! This module owns ONE attempt at priming a single account (the "Scheduled prime" core flow
//! in docs/account-switcher/brainstorms/auto-session-prime.md, decision points D1–D4). The
//! scheduler in `app_state` decides *when* to call it and records the outcome.
//!
//! Verified upstream facts (see the prototype `scripts/session-prime-today.sh`):
//!   - Claude: POST /v1/messages with the Claude Code system preamble, model haiku.
//!   - Codex:  POST /backend-api/codex/responses, model gpt-5.5, needs ChatGPT-Account-Id.
//!   - A prime only opens a NEW window if the old one already reset; otherwise the request
//!     falls into the running window (→ D2 HOLD). So we read `reset_at` before sending.

use crate::models::ToolId;
use crate::quota;
use serde_json::json;
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// The cheapest model that the subscription endpoint accepts for each tool.
const CLAUDE_PRIME_MODEL: &str = "claude-haiku-4-5-20251001";
const CODEX_PRIME_MODEL: &str = "gpt-5.5";
const CLAUDE_CODE_SYSTEM_PREAMBLE: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Delay used only when a caller explicitly asks one bounded invocation to retry. Durable
/// scheduled retries are orchestrated by app_state and persisted in prime-runtime.json.
pub const SEND_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);
/// Confirm-retry policy (D4): after a 2xx send, re-read the live window until `reset_at` moves.
pub const CONFIRM_MAX_TRIES: u32 = 5;
pub const CONFIRM_RETRY_DELAY: Duration = Duration::from_secs(30);
/// Codex needs two observations: a rolling reset advances with wall time, while a real session's
/// reset remains fixed. 75s is safely above the 60s minimum proof gap while staying inside one
/// short caffeinated wake transaction.
pub const CODEX_OBSERVATION_DELAY: Duration = Duration::from_secs(75);
pub const CODEX_FIXED_RESET_EPSILON_SECONDS: i64 = 15;
/// Hard cap on how long a prime CLI invocation may run before we kill it (a hung CLI must never
/// hold the prime worker — see the scheduler's overlap guard).
pub const CLI_TIMEOUT: Duration = Duration::from_secs(120);

/// The outcome of one prime attempt, mapped to the brainstorm's log wording by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimeOutcome {
    /// Sent + confirmed the 5h window moved to a new reset. Carries the new `reset_at` (ISO).
    Success { new_reset_at: String },
    /// The old 5h window is still active; do not prime yet. Carries that window's `reset_at`.
    Hold { reset_at: String },
    /// Account has no valid token (expired / logged out).
    SkipNoToken,
    /// Couldn't establish the current window state (read error / unparseable / inconclusive data),
    /// so we did NOT send — failing closed rather than priming into an unknown window. Transient;
    /// the next scheduled tick retries.
    SkipUnknownState,
    /// The bounded send burst failed. The persisted scheduler decides whether the deadline permits
    /// another burst.
    FailSend { reason: String },
    /// Send was OK but the window never moved after `CONFIRM_MAX_TRIES`.
    FailUnconfirmed,
}

/// True when this account can be primed at all: subscription (OAuth) Claude/Codex only.
/// API-proxy accounts have no 5h window; Antigravity is unsupported.
pub fn is_prime_eligible(tool_id: &ToolId, has_api_provider: bool) -> bool {
    !has_api_provider && matches!(tool_id, ToolId::Claude | ToolId::Codex)
}

/// Run one bounded prime burst. The crash-safety hook is invoked after precheck succeeds and
/// immediately
/// before the first external send. The scheduler uses it to durably persist `Confirming`,
/// `baseline_reset_at`, and `last_send_at`; a failed hook aborts the send.
pub fn prime_account_with_hook(
    tool_id: &ToolId,
    config_dir: &Path,
    binary: Option<&Path>,
    send_attempts: u32,
    mut sleeper: impl FnMut(Duration),
    mut before_send: impl FnMut(Option<&str>) -> Result<(), String>,
) -> PrimeOutcome {
    // Hold the Mac awake for the whole attempt. A pmset wake only buys a brief awake window before
    // macOS idle-sleeps again; D3's retries (up to 5 × 5') and D4's confirm polls can outlast it,
    // and a prime that started right after a pmset wake must not be cut off by a re-sleep. Dropped
    // at function exit (any return path). No-op when caffeinate is missing (non-macOS / stripped).
    let _awake = CaffeinateGuard::start();

    // D1 — token must be valid.
    if read_token(tool_id, config_dir, binary).is_none() {
        return PrimeOutcome::SkipNoToken;
    }

    // D2 — if a REAL 5h window is still running, the prime would land inside it → HOLD.
    // Single-snapshot classification (no probe): sending "hi" is as cheap and harmless as typing it
    // in the terminal, so we only HOLD when the window is DEFINITELY a real anchored one. Codex's
    // `Ambiguous` (reset ≈ now + 5h — rolling for a low-usage account, which never anchors from a
    // bare "hi") falls through to send, matching the terminal's behaviour.
    let before = match quota::read_live_five_hour(tool_id, config_dir) {
        Ok(window) => window,
        Err(_) => return PrimeOutcome::SkipUnknownState,
    };
    let before_reset = before.reset_at.clone();
    let state = quota::classify_five_hour(tool_id, &before);
    match state {
        // A real anchored window is running → don't pile a prime into it. `Anchored` is only
        // produced from a parseable future reset, so `before_reset` is present.
        quota::WindowState::Anchored => match &before_reset {
            Some(reset_at) => {
                return PrimeOutcome::Hold {
                    reset_at: reset_at.clone(),
                }
            }
            None => return PrimeOutcome::SkipUnknownState,
        },
        // We can't establish the window state → fail CLOSED: do NOT send blindly. The next
        // scheduled tick retries once the read recovers.
        quota::WindowState::Unknown => return PrimeOutcome::SkipUnknownState,
        // Primeable (ended/no window) or Ambiguous (Codex rolling) → send.
        quota::WindowState::Primeable | quota::WindowState::Ambiguous => {}
    }

    // D3 — send "hi", retrying on failure up to `send_attempts` times.
    if let Err(reason) = before_send(before_reset.as_deref()) {
        return PrimeOutcome::FailSend { reason };
    }
    let attempts = send_attempts.max(1);
    let mut last_reason = String::new();
    let mut sent = false;
    for attempt in 1..=attempts {
        match send_hi(tool_id, config_dir, binary) {
            Ok(()) => {
                sent = true;
                break;
            }
            Err(reason) => {
                last_reason = reason;
                if attempt < attempts {
                    sleeper(SEND_RETRY_DELAY);
                }
            }
        }
    }
    if !sent {
        return PrimeOutcome::FailSend {
            reason: last_reason,
        };
    }

    // D4 — confirm the prime took.
    //
    // Provider-aware:
    //   - Codex: a bare "hi" does NOT reliably anchor the 5h window on a low-usage account. A
    //     single snapshot around now+5h is ambiguous, so observe twice: rolling resets advance with
    //     wall time; an active session's reset stays fixed.
    //   - Claude: `reset_at` is a stable anchor, so we can (and should) confirm the window actually
    //     moved to a new future reset before claiming success. Poll a few times — the provider may
    //     take a few seconds to refresh.
    if matches!(tool_id, ToolId::Codex) {
        let first = quota::read_live_five_hour(tool_id, config_dir).ok();
        sleeper(CODEX_OBSERVATION_DELAY);
        let second = quota::read_live_five_hour(tool_id, config_dir).ok();
        if let (Some(first), Some(second)) = (first, second) {
            if let (Some(first_reset), Some(second_reset)) =
                (first.reset_at.as_deref(), second.reset_at.as_deref())
            {
                if codex_reset_is_fixed(first_reset, second_reset) {
                    return PrimeOutcome::Success {
                        new_reset_at: second_reset.to_string(),
                    };
                }
            }
        }
        return PrimeOutcome::FailUnconfirmed;
    }
    for _ in 0..CONFIRM_MAX_TRIES {
        sleeper(CONFIRM_RETRY_DELAY);
        if let Ok(window) = quota::read_live_five_hour(tool_id, config_dir) {
            if let Some(new_reset) = &window.reset_at {
                if window_moved(before_reset.as_deref(), new_reset) {
                    return PrimeOutcome::Success {
                        new_reset_at: new_reset.clone(),
                    };
                }
            }
        }
    }
    PrimeOutcome::FailUnconfirmed
}

/// Resume an attempt that may have crashed after its durable `Confirming` marker was written.
/// This path never sends: it only proves whether a real session is active, preventing a blind
/// duplicate request after restart.
pub fn confirm_active_session(
    tool_id: &ToolId,
    config_dir: &Path,
    baseline_reset_at: Option<&str>,
    mut sleeper: impl FnMut(Duration),
) -> PrimeOutcome {
    match tool_id {
        ToolId::Claude => {
            let Ok(window) = quota::read_live_five_hour(tool_id, config_dir) else {
                return PrimeOutcome::SkipUnknownState;
            };
            match window.reset_at {
                Some(reset_at) if claude_reset_confirms(baseline_reset_at, &reset_at) => {
                    PrimeOutcome::Success {
                        new_reset_at: reset_at,
                    }
                }
                _ => PrimeOutcome::FailUnconfirmed,
            }
        }
        ToolId::Codex => {
            let first = quota::read_live_five_hour(tool_id, config_dir).ok();
            sleeper(CODEX_OBSERVATION_DELAY);
            let second = quota::read_live_five_hour(tool_id, config_dir).ok();
            if let (Some(first), Some(second)) = (first, second) {
                if let (Some(first_reset), Some(second_reset)) =
                    (first.reset_at.as_deref(), second.reset_at.as_deref())
                {
                    if codex_reset_is_fixed(first_reset, second_reset) {
                        return PrimeOutcome::Success {
                            new_reset_at: second_reset.to_string(),
                        };
                    }
                }
            }
            PrimeOutcome::FailUnconfirmed
        }
        ToolId::Antigravity => PrimeOutcome::SkipUnknownState,
    }
}

fn claude_reset_confirms(baseline: Option<&str>, candidate: &str) -> bool {
    is_future(candidate) && baseline.is_none_or(|before| before != candidate)
}

fn codex_reset_is_fixed(first: &str, second: &str) -> bool {
    let Ok(first) = chrono::DateTime::parse_from_rfc3339(first) else {
        return false;
    };
    let Ok(second) = chrono::DateTime::parse_from_rfc3339(second) else {
        return false;
    };
    second > chrono::Utc::now()
        && (second.timestamp() - first.timestamp()).abs() <= CODEX_FIXED_RESET_EPSILON_SECONDS
}

/// Keeps the Mac awake (no idle sleep) for as long as it is alive, by holding a child
/// `caffeinate` process. Dropping it kills the child, letting the Mac sleep normally again.
struct CaffeinateGuard(Option<std::process::Child>);

impl CaffeinateGuard {
    /// Spawn `caffeinate -i -w <our pid>` (prevent idle system sleep, and self-exit if we die).
    /// The `-w` watch is belt-and-suspenders against an orphaned caffeinate holding the Mac awake
    /// forever should the app crash before Drop runs. On any failure — including non-macOS where
    /// the binary doesn't exist — returns an inert guard so callers stay unconditional.
    fn start() -> Self {
        let child = std::process::Command::new("caffeinate")
            .args(["-i", "-w", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
        CaffeinateGuard(child)
    }
}

impl Drop for CaffeinateGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn read_token(tool_id: &ToolId, config_dir: &Path, binary: Option<&Path>) -> Option<String> {
    match tool_id {
        ToolId::Claude => quota::claude_oauth_token_fresh(config_dir, binary),
        ToolId::Codex => quota::codex_access_token_fresh(config_dir),
        ToolId::Antigravity => None,
    }
}

/// Send a minimal "hi" to open a fresh window.
///
/// Claude uses its hardened CLI invocation so the CLI can repair stale Keychain/OAuth state.
/// Codex always uses the direct endpoint: even an ephemeral, read-only `codex exec` starts Git
/// discovery under its macOS sandbox, which preflights Desktop/Documents/Downloads/Media Library
/// and causes permission prompts attributed to this app. Codex's token can be refreshed directly,
/// so starting the full agent runtime is unnecessary.
fn send_hi(tool_id: &ToolId, config_dir: &Path, binary: Option<&Path>) -> Result<(), String> {
    if !uses_cli_for_prime(tool_id) {
        return send_hi_http(tool_id, config_dir);
    }
    if let Some(binary) = binary {
        match send_hi_cli(tool_id, config_dir, binary) {
            Ok(()) => return Ok(()),
            // CLI binary exists but the run failed (not a "missing binary"): surface that error
            // rather than silently masking it with an HTTP attempt that shares the same auth.
            Err(reason) => return Err(reason),
        }
    }
    send_hi_http(tool_id, config_dir)
}

fn uses_cli_for_prime(tool_id: &ToolId) -> bool {
    matches!(tool_id, ToolId::Claude)
}

/// Prime by running the account's CLI non-interactively, with the account's config dir in the
/// environment so the CLI uses the right profile/token. Exit 0 = success.
fn send_hi_cli(tool_id: &ToolId, config_dir: &Path, binary: &Path) -> Result<(), String> {
    use std::process::Command;
    let mut command = Command::new(binary);
    // Finder/Dock apps and LaunchDaemons commonly inherit only
    // `/usr/bin:/bin:/usr/sbin:/sbin`. npm-installed CLIs are scripts with an
    // `#!/usr/bin/env node` shebang, so spawning the configured `codex` path can
    // succeed while the script itself exits 127 because `env` cannot find Node.
    // Supply the same deterministic install locations used by tool detection,
    // while preserving any useful entries inherited from the user's session.
    command.env("PATH", cli_path(binary));
    match tool_id {
        ToolId::Claude => {
            command
                .args(quota::CLAUDE_BACKGROUND_ARGS)
                .env("CLAUDE_CONFIG_DIR", config_dir)
                // A background prime needs auth + one API request only. Safe mode alone still lets
                // Claude initialise its built-in tool/sandbox layer, which preflights Desktop,
                // Documents, Downloads and Media Library through macOS TCC. Disable every context
                // and tool source explicitly so the child remains an API-only OAuth invocation.
                .env("CLAUDE_CODE_SAFE_MODE", "1")
                .current_dir(config_dir);
        }
        ToolId::Codex => {
            command
                .args([
                    "exec",
                    "--skip-git-repo-check",
                    "--ephemeral",
                    "--ignore-user-config",
                    "--ignore-rules",
                    "--sandbox",
                    "read-only",
                    "hi",
                ])
                .env("CODEX_HOME", config_dir)
                // The GUI process normally starts with `/` as cwd. `codex exec` rejects that as an
                // untrusted non-repository unless explicitly allowed. Use the account profile as a
                // harmless read-only working root and do not persist the one-message session.
                .current_dir(config_dir);
        }
        ToolId::Antigravity => return Err("antigravity unsupported".to_string()),
    }
    // Don't inherit a TTY. stdout is irrelevant, but retain stderr so failures are diagnosable
    // instead of surfacing only as "CLI exit 1".
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("CLI: {e}"))?;
    let mut stderr = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            use std::io::Read;
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).trim().to_string()
        })
    });

    // Hard timeout: a CLI that hangs (network stall, auth/update prompt) must NOT hold the prime
    // worker forever — that would block the scheduler's overlap guard and skip every later prime.
    let deadline = std::time::Instant::now() + CLI_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let detail = stderr
                    .take()
                    .and_then(|reader| reader.join().ok())
                    .map(|text| concise_cli_error(&text))
                    .filter(|text| !text.is_empty());
                return if status.success() {
                    Ok(())
                } else {
                    let code = status.code().unwrap_or(-1);
                    Err(detail
                        .map(|detail| format!("CLI exit {code}: {detail}"))
                        .unwrap_or_else(|| format!("CLI exit {code}")))
                };
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("CLI timeout".to_string());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("CLI: {e}")),
        }
    }
}

fn concise_cli_error(stderr: &str) -> String {
    const MAX_CHARS: usize = 240;
    let text = stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "Reading additional input from stdin...")
        .collect::<Vec<_>>()
        .join(" · ");
    if text.chars().count() <= MAX_CHARS {
        text
    } else {
        format!("{}…", text.chars().take(MAX_CHARS).collect::<String>())
    }
}

fn cli_path(binary: &Path) -> OsString {
    use std::collections::HashSet;

    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |path: std::path::PathBuf| {
        if seen.insert(path.clone()) {
            paths.push(path);
        }
    };

    if let Some(parent) = binary.parent() {
        push(parent.to_path_buf());
    }
    for path in crate::tools::common_bin_dirs() {
        push(path);
    }
    if let Some(inherited) = std::env::var_os("PATH") {
        for path in std::env::split_paths(&inherited) {
            push(path);
        }
    }

    std::env::join_paths(paths)
        .unwrap_or_else(|_| OsString::from("/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"))
}

/// POST a minimal "hi" directly. Returns Ok(()) on a 2xx, Err(reason) otherwise.
fn send_hi_http(tool_id: &ToolId, config_dir: &Path) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("client: {e}"))?;

    let response = match tool_id {
        ToolId::Claude => {
            // No explicit binary here; claude_oauth_token_fresh resolves `claude` via command_path
            // for the token refresh (a bare PATH lookup fails under a GUI .app's minimal PATH).
            let token = quota::claude_oauth_token_fresh(config_dir, None)
                .ok_or_else(|| "token".to_string())?;
            let version = quota::claude_version().unwrap_or_else(|| "2.0.0".to_string());
            let body = json!({
                "model": CLAUDE_PRIME_MODEL,
                "max_tokens": 1,
                "system": [{"type": "text", "text": CLAUDE_CODE_SYSTEM_PREAMBLE}],
                "messages": [{"role": "user", "content": "hi"}],
            });
            client
                .post("https://api.anthropic.com/v1/messages")
                .bearer_auth(token)
                .header("anthropic-version", "2023-06-01")
                .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
                .header(
                    "User-Agent",
                    format!("claude-cli/{version} (external, sdk-cli)"),
                )
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
        }
        ToolId::Codex => {
            let token =
                quota::codex_access_token_fresh(config_dir).ok_or_else(|| "token".to_string())?;
            let body = json!({
                "model": CODEX_PRIME_MODEL,
                "instructions": "You are a helpful coding assistant.",
                "input": [{
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": "hi"}],
                }],
                "store": false,
                "stream": true,
            });
            let mut request = client
                .post("https://chatgpt.com/backend-api/codex/responses")
                .bearer_auth(token)
                .header("Accept", "text/event-stream")
                .header("User-Agent", "codex_cli_rs/0.0.0")
                .header("OpenAI-Beta", "responses=experimental")
                .header("originator", "codex_cli_rs")
                .header("Content-Type", "application/json")
                .json(&body);
            if let Some(account_id) = quota::codex_account_id(config_dir) {
                request = request.header("ChatGPT-Account-Id", account_id);
            }
            request.send()
        }
        ToolId::Antigravity => return Err("antigravity unsupported".to_string()),
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                Ok(())
            } else {
                Err(format!("HTTP {}", status.as_u16()))
            }
        }
        Err(e) if e.is_timeout() => Err("timeout".to_string()),
        Err(e) => Err(format!("network: {e}")),
    }
}

/// `reset_at` (ISO 8601) is strictly after now.
fn is_future(reset_at: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(reset_at)
        .map(|t| t > chrono::Utc::now())
        .unwrap_or(false)
}

/// The window moved to a new reset: it's in the future AND differs from the pre-prime value.
fn window_moved(before: Option<&str>, after: &str) -> bool {
    if !is_future(after) {
        return false;
    }
    match before {
        Some(before) => before != after,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn eligibility_excludes_api_and_antigravity() {
        assert!(is_prime_eligible(&ToolId::Claude, false));
        assert!(is_prime_eligible(&ToolId::Codex, false));
        assert!(!is_prime_eligible(&ToolId::Claude, true)); // API-proxy account
        assert!(!is_prime_eligible(&ToolId::Antigravity, false));
    }

    #[test]
    fn codex_prime_bypasses_agent_cli() {
        assert!(!uses_cli_for_prime(&ToolId::Codex));
        assert!(uses_cli_for_prime(&ToolId::Claude));
    }

    #[test]
    fn window_moved_detects_new_reset() {
        let past = "2000-01-01T00:00:00Z";
        let future_a = "2999-01-01T00:00:00Z";
        let future_b = "2999-06-01T00:00:00Z";
        // No prior window → any future reset counts as moved.
        assert!(window_moved(None, future_a));
        // Same future reset → not moved (window didn't change).
        assert!(!window_moved(Some(future_a), future_a));
        // Different future reset → moved.
        assert!(window_moved(Some(future_a), future_b));
        // New value in the past → not a valid new window.
        assert!(!window_moved(Some(future_a), past));
    }

    #[test]
    fn codex_fixed_reset_proof_rejects_rolling_value() {
        let future = chrono::Utc::now() + chrono::Duration::hours(5);
        let fixed_a = future.to_rfc3339();
        let fixed_b = (future + chrono::Duration::seconds(10)).to_rfc3339();
        let rolling = (future + chrono::Duration::seconds(75)).to_rfc3339();
        assert!(codex_reset_is_fixed(&fixed_a, &fixed_b));
        assert!(!codex_reset_is_fixed(&fixed_a, &rolling));
    }

    #[test]
    fn codex_fixed_reset_boundary_is_fifteen_seconds() {
        let future = chrono::Utc::now() + chrono::Duration::hours(5);
        assert!(codex_reset_is_fixed(
            &future.to_rfc3339(),
            &(future + chrono::Duration::seconds(15)).to_rfc3339()
        ));
        assert!(!codex_reset_is_fixed(
            &future.to_rfc3339(),
            &(future + chrono::Duration::seconds(16)).to_rfc3339()
        ));
    }

    #[test]
    fn claude_resume_requires_reset_movement_from_baseline() {
        let baseline = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let moved = (chrono::Utc::now() + chrono::Duration::hours(5)).to_rfc3339();
        assert!(!claude_reset_confirms(Some(&baseline), &baseline));
        assert!(claude_reset_confirms(Some(&baseline), &moved));
        assert!(claude_reset_confirms(None, &moved)); // valid empty precheck
    }

    #[test]
    fn cli_path_includes_runtime_locations_for_env_shebangs() {
        let path = cli_path(Path::new("/Users/test/.npm-global/bin/codex"));
        let entries = std::env::split_paths(&path).collect::<Vec<PathBuf>>();
        assert!(entries.contains(&PathBuf::from("/Users/test/.npm-global/bin")));
        assert!(entries.contains(&PathBuf::from("/opt/homebrew/bin")));
        assert!(entries.contains(&PathBuf::from("/usr/local/bin")));
        assert!(entries.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn cli_error_is_short_and_drops_codex_stdin_noise() {
        let error = concise_cli_error(
            "Reading additional input from stdin...\nNot inside a trusted directory\n",
        );
        assert_eq!(error, "Not inside a trusted directory");
        assert!(concise_cli_error(&"x".repeat(500)).chars().count() <= 241);
    }
}

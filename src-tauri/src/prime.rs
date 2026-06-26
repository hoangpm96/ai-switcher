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
/// Claude confirm (D4): after a 2xx send, poll the live window until it proves a freshly anchored
/// session. Two signals (preferred → fallback):
///   1. `limits[kind == "session"].is_active == true` with a future `reset_at` — the provider flips
///      this the moment the new 5h window opens, BEFORE `reset_at` settles to a new value. Fast and
///      reliable. (D2 only sends when no real window was anchored, so an active session here is the
///      one our "hi" just opened.)
///   2. `reset_at` moved to a new future value vs the pre-send baseline — the original signal, kept as
///      a fallback for payloads that don't carry `is_active`.
///
/// Read FIRST, then sleep only if not yet confirmed, so the common "opened instantly" case returns in
/// one read instead of after a fixed delay. Bounded by a max poll count AND a wall-clock budget (each
/// read is a ~1s HTTP call) so a confirmation never overruns the scheduler's per-tick proof budget
/// (`PRIME_PROOF_BUDGET_SECONDS`). On give-up the scheduler's 5-minute retry loop re-confirms.
pub const CONFIRM_MAX_TRIES: u32 = 8;
pub const CONFIRM_RETRY_DELAY: Duration = Duration::from_secs(10);
pub const CONFIRM_TOTAL_BUDGET: Duration = Duration::from_secs(90);
/// Codex confirm (D4): after a 2xx send, poll the live window until it reads as a clearly-anchored
/// real session, tolerating a still-rolling reset or a transient read failure. The poll is bounded by
/// BOTH a max poll count AND a hard wall-clock budget — each `read_live_five_hour` makes a `curl` call
/// (up to 20s), so counting sleeps alone undercounts; `CODEX_CONFIRM_TOTAL_BUDGET` caps the real
/// elapsed time so a confirmation can never overrun the scheduler's per-tick proof budget
/// (`PRIME_PROOF_BUDGET_SECONDS`). On give-up the scheduler's 5-minute retry loop re-confirms.
///
/// Codex anchors a window after a single "hi" (verified live: a 1% send anchored in ~26s), but the
/// `reset_at` snap from rolling → fixed has a HIGHLY variable delay — some sends never settle within
/// a couple minutes. So we poll DENSELY (every 10s) across the largest budget that still fits the
/// proof budget, to catch the snap whenever it lands; if a tick gives up, the scheduler's retry loop
/// re-confirms on the next tick (the real "stretch": confirmation spans several ticks, not one long
/// inline wait, which the proof budget forbids). Confirmation reads only — it never sends again, so a
/// longer/denser poll costs no extra quota (the original "hi" already cost its ~1%).
pub const CODEX_CONFIRM_POLL_DELAY: Duration = Duration::from_secs(10);
pub const CODEX_CONFIRM_MAX_POLLS: u32 = 12;
pub const CODEX_CONFIRM_TOTAL_BUDGET: Duration = Duration::from_secs(125);
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

    // D1 — token must be valid AND not expired. For Claude, renew an expired access token first:
    // the prime runs at an early-morning hour when no `claude` CLI session is using the account, so
    // rotating the one-time-use refresh token here is safe (the daytime UI quota path must NOT do
    // this — see `claude_oauth_token_fresh`). This closes the 2026-06-25 failure where a present-but-
    // expired token passed D1, then 401'd during the window read / confirm and reported a confusing
    // "couldn't confirm session". Reads `expiresAt` offline; only hits the token endpoint when stale.
    if matches!(tool_id, ToolId::Claude) {
        match quota::ensure_fresh_claude_token(config_dir) {
            quota::TokenRefresh::Ready => {}
            quota::TokenRefresh::NoToken => return PrimeOutcome::SkipNoToken,
            // 429 / transient failure: don't send (would 401), and don't hammer the rate-limit-
            // sensitive token endpoint. Fail open as retryable so the scheduler's 5-minute loop
            // re-attempts once the throttle clears.
            quota::TokenRefresh::RateLimited | quota::TokenRefresh::Failed => {
                return PrimeOutcome::SkipUnknownState
            }
        }
    } else if read_token(tool_id, config_dir).is_none() {
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
    let before_active = before.is_active;
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
    //   - Codex: sending "hi" DOES anchor the 5h window (verified live), but the anchor lands with a
    //     variable delay: the `/wham/usage` reset stays "rolling" (≈ now + 5h, advancing with wall
    //     time) for a moment, then snaps to a FIXED epoch that counts down. We confirm by POLLING the
    //     window until it reads as `Anchored` (reset clearly inside now+5h, i.e. not the rolling
    //     signature). This is robust to both a slow anchor and a transient read failure — a failed
    //     read just retries on the next poll instead of aborting the whole confirmation (the old
    //     two-observation drift proof reported FAIL whenever one of its two reads slipped).
    //   - Claude: `reset_at` is a stable anchor, so we confirm the window actually moved to a new
    //     future reset before claiming success. Poll a few times — the provider may take a few
    //     seconds to refresh.
    if matches!(tool_id, ToolId::Codex) {
        return match codex_confirm_anchored(config_dir, &mut sleeper) {
            Some(new_reset_at) => PrimeOutcome::Success { new_reset_at },
            None => PrimeOutcome::FailUnconfirmed,
        };
    }
    match claude_confirm_anchored(config_dir, before_reset.as_deref(), before_active, &mut sleeper) {
        Some(new_reset_at) => PrimeOutcome::Success { new_reset_at },
        None => PrimeOutcome::FailUnconfirmed,
    }
}

/// Poll the Claude 5h window until it proves a freshly anchored session, or the inline budget runs
/// out. See `CONFIRM_MAX_TRIES` for the two success signals and the read-first rationale. Returns the
/// new window's `reset_at` on success.
fn claude_confirm_anchored(
    config_dir: &Path,
    baseline_reset_at: Option<&str>,
    baseline_active: Option<bool>,
    mut sleeper: impl FnMut(Duration),
) -> Option<String> {
    let started = std::time::Instant::now();
    for poll in 0..CONFIRM_MAX_TRIES {
        if poll > 0 {
            // Stop before a sleep that would push us past the wall-clock budget (each read below also
            // makes an HTTP call), then take one final read after the loop is no longer worth sleeping.
            if started.elapsed() + CONFIRM_RETRY_DELAY >= CONFIRM_TOTAL_BUDGET {
                break;
            }
            sleeper(CONFIRM_RETRY_DELAY);
        }
        if let Some(reset_at) = claude_anchored_reset(config_dir, baseline_reset_at, baseline_active)
        {
            return Some(reset_at);
        }
        if started.elapsed() >= CONFIRM_TOTAL_BUDGET {
            break;
        }
    }
    None
}

/// One read of the Claude window → `Some(reset_at)` if it proves a freshly anchored session.
fn claude_anchored_reset(
    config_dir: &Path,
    baseline_reset_at: Option<&str>,
    baseline_active: Option<bool>,
) -> Option<String> {
    let window = quota::read_live_five_hour(&ToolId::Claude, config_dir).ok()?;
    let reset_at = window.reset_at?;
    claude_reset_confirms(baseline_reset_at, baseline_active, &reset_at, window.is_active)
        .then_some(reset_at)
}

/// Whether a Claude window read proves a session that THIS prime freshly opened.
///
/// Signal 1 — newly active: the session is now active with a future reset AND it was NOT already
/// active before we sent (`baseline_active != Some(true)`). The transition is what proves our "hi"
/// opened the window; an already-active baseline with an unmoved reset must NOT count, or clicking
/// "Prime now" on a still-running window would falsely report a fresh window and persist the old
/// reset. (D2 already HOLDs a clearly-anchored window upstream; this is defense in depth so the
/// predicate is correct regardless of caller — e.g. the crash-resume path.)
/// Signal 2 — reset moved: the reset advanced to a new future value vs the pre-send baseline. This
/// stands on its own (a moved reset is unambiguous proof) even if `is_active` is absent.
fn claude_reset_confirms(
    baseline: Option<&str>,
    baseline_active: Option<bool>,
    reset_at: &str,
    is_active: Option<bool>,
) -> bool {
    let newly_active =
        is_active == Some(true) && baseline_active != Some(true) && is_future(reset_at);
    newly_active || window_moved(baseline, reset_at)
}

/// Poll the Codex 5h window until it proves a real anchored session, or the inline budget runs out.
///
/// Two independent success signals (verified live 2026-06-23):
///   1. `WindowState::Anchored` — the reset is clearly inside now+5h (a window that started a while
///      ago). Fast path when a real session was already running.
///   2. **Stable reset across consecutive reads** — a freshly anchored window's reset is a FIXED epoch
///      ≈ `send_time + 5h`, so for the first ~90s it is still "near now+5h" and classifies as
///      `Ambiguous`, indistinguishable from rolling by a single snapshot. But a ROLLING reset ADVANCES
///      with wall time (`now + 5h` recomputed every read) while an ANCHORED reset stays put. So if two
///      consecutive reads (≥ one poll interval apart) report the SAME future reset epoch, the window
///      is anchored — even while still numerically near now+5h. This catches the common "hi just
///      anchored it" case in ~one poll interval instead of waiting the full 90s for signal (1).
///
/// A rolling window or a transient read failure simply keeps polling. Bounded by BOTH a max poll
/// count and a hard wall-clock budget (each read costs up to a 20s curl), staying inside the
/// scheduler's per-tick proof budget; on give-up the scheduler's 5-minute retry loop re-confirms.
fn codex_confirm_anchored(
    config_dir: &Path,
    mut sleeper: impl FnMut(Duration),
) -> Option<String> {
    let started = std::time::Instant::now();
    let mut previous_epoch: Option<i64> = None;
    for poll in 0..CODEX_CONFIRM_MAX_POLLS {
        if poll > 0 {
            // Stop before a sleep that would push us past the wall-clock budget. Each read below also
            // costs up to a 20s curl, so the count cap alone is not enough to bound real elapsed time.
            if started.elapsed() + CODEX_CONFIRM_POLL_DELAY >= CODEX_CONFIRM_TOTAL_BUDGET {
                break;
            }
            sleeper(CODEX_CONFIRM_POLL_DELAY);
        }
        if let Ok(window) = quota::read_live_five_hour(&ToolId::Codex, config_dir) {
            // Signal 1: clearly anchored (reset far from now+5h).
            if matches!(
                quota::classify_five_hour(&ToolId::Codex, &window),
                quota::WindowState::Anchored
            ) {
                // `Anchored` is only produced from a parseable future reset, so this is present.
                if let Some(reset_at) = window.reset_at {
                    return Some(reset_at);
                }
            }
            // Signal 2: a future reset whose epoch is UNCHANGED since the previous poll → fixed →
            // anchored. (A rolling reset would have advanced by ≈ the poll interval.)
            if let Some(reset_epoch) = window
                .reset_at
                .as_deref()
                .and_then(codex_reset_epoch_if_future)
            {
                if previous_epoch == Some(reset_epoch) {
                    return window.reset_at;
                }
                previous_epoch = Some(reset_epoch);
            }
        }
        if started.elapsed() >= CODEX_CONFIRM_TOTAL_BUDGET {
            break;
        }
    }
    None
}

/// Parse a Codex `reset_at` (RFC3339) to a unix epoch, but only if it is in the future. A past or
/// unparseable reset is not a live window to confirm against.
fn codex_reset_epoch_if_future(reset_at: &str) -> Option<i64> {
    let reset = chrono::DateTime::parse_from_rfc3339(reset_at).ok()?;
    (reset > chrono::Utc::now()).then(|| reset.timestamp())
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
            // Same anchored-session poll as the inline D4 path (newly-active session OR a moved
            // reset). A `Confirming` marker only exists because a send happened, which only happens
            // after D2 confirmed no window was anchored — so the baseline was inactive. Passing
            // `Some(false)` lets the newly-active signal count without risking a false positive.
            match claude_confirm_anchored(config_dir, baseline_reset_at, Some(false), &mut sleeper) {
                Some(new_reset_at) => PrimeOutcome::Success { new_reset_at },
                None => PrimeOutcome::FailUnconfirmed,
            }
        }
        ToolId::Codex => {
            // Resume never re-sends: it only checks whether the window has anchored since the send
            // that wrote the `Confirming` marker. Same anchored-signature poll as the inline D4 path
            // (`baseline_reset_at` is unused for Codex — an anchored reset is proof on its own).
            let _ = baseline_reset_at;
            match codex_confirm_anchored(config_dir, &mut sleeper) {
                Some(new_reset_at) => PrimeOutcome::Success { new_reset_at },
                None => PrimeOutcome::FailUnconfirmed,
            }
        }
        ToolId::Antigravity => PrimeOutcome::SkipUnknownState,
    }
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

fn read_token(tool_id: &ToolId, config_dir: &Path) -> Option<String> {
    match tool_id {
        ToolId::Claude => quota::claude_oauth_token_fresh(config_dir),
        ToolId::Codex => quota::codex_access_token_fresh(config_dir),
        ToolId::Antigravity => None,
    }
}

/// Send a minimal "hi" to open a fresh window.
///
/// Prime directly over HTTP whenever possible. Starting the full Claude/Codex agent runtime for a
/// one-token background request can still initialise sandbox/tool preflights and trigger macOS TCC
/// prompts attributed to this app.
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
    matches!(tool_id, ToolId::Antigravity)
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
            // The token was already renewed if needed at D1 (`ensure_fresh_claude_token`), so this
            // is a plain read of the current (fresh) access token.
            let token = quota::claude_oauth_token_fresh(config_dir)
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
    fn subscription_primes_bypass_agent_cli() {
        assert!(!uses_cli_for_prime(&ToolId::Codex));
        assert!(!uses_cli_for_prime(&ToolId::Claude));
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
    fn codex_confirm_uses_anchored_signature_not_rolling() {
        // The Codex confirm relies on `classify_five_hour`: a rolling reset (≈ now+5h) must NOT count
        // as confirmation, while a reset clearly inside the window (anchored) must. This is the exact
        // signal `codex_confirm_anchored` polls for.
        use crate::models::QuotaWindow;
        let now = chrono::Utc::now();
        let rolling = QuotaWindow {
            label: "5h".into(),
            percent_used: Some(1.0),
            reset_at: Some((now + chrono::Duration::seconds(18000)).to_rfc3339()),
            is_active: None,
        };
        let anchored = QuotaWindow {
            label: "5h".into(),
            percent_used: Some(1.0),
            // Anchored a while ago → reset is well inside now+5h (verified live: reset−now shrinks).
            reset_at: Some((now + chrono::Duration::seconds(16000)).to_rfc3339()),
            is_active: None,
        };
        assert_eq!(
            quota::classify_five_hour(&ToolId::Codex, &rolling),
            quota::WindowState::Ambiguous
        );
        assert_eq!(
            quota::classify_five_hour(&ToolId::Codex, &anchored),
            quota::WindowState::Anchored
        );
    }

    #[test]
    fn codex_reset_epoch_if_future_rejects_past_and_unparseable() {
        let future = (chrono::Utc::now() + chrono::Duration::hours(5)).to_rfc3339();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert!(codex_reset_epoch_if_future(&future).is_some());
        assert!(codex_reset_epoch_if_future(&past).is_none());
        assert!(codex_reset_epoch_if_future("not-a-timestamp").is_none());
    }

    #[test]
    fn codex_stable_future_reset_proves_anchor_while_still_near_full_window() {
        // The core of the stable-epoch signal: a freshly anchored reset sits ≈ now+5h (so the
        // single-snapshot classifier still says Ambiguous), yet because the epoch is FIXED, two reads
        // a poll apart return the SAME value — whereas a rolling reset advances by the elapsed time.
        let now = chrono::Utc::now();
        let anchored_epoch = (now + chrono::Duration::seconds(17_990)).to_rfc3339();
        // Same epoch read twice → equal → anchored.
        let e1 = codex_reset_epoch_if_future(&anchored_epoch).unwrap();
        let e2 = codex_reset_epoch_if_future(&anchored_epoch).unwrap();
        assert_eq!(e1, e2, "fixed reset must read identically across polls");
        // A rolling reset 15s later would be 15s larger → not equal.
        let rolling_later = (now + chrono::Duration::seconds(17_990 + 15)).to_rfc3339();
        assert_ne!(e1, codex_reset_epoch_if_future(&rolling_later).unwrap());
    }

    #[test]
    fn codex_confirm_poll_budget_fits_proof_window() {
        // The inline poll must finish within the scheduler's per-tick proof budget so a tick is never
        // cut off mid-confirmation (PRIME_PROOF_BUDGET_SECONDS = 150 in app_state). The hard cap is
        // the wall-clock budget; account for one in-flight read (~20s curl) on top of it.
        const PROOF_BUDGET: u64 = 150;
        const READ_LATENCY_HEADROOM: u64 = 20;
        assert!(
            CODEX_CONFIRM_TOTAL_BUDGET.as_secs() + READ_LATENCY_HEADROOM <= PROOF_BUDGET,
            "total budget + one read can overrun the proof window"
        );
        assert!(CODEX_CONFIRM_MAX_POLLS >= 2, "must poll more than once");
    }

    #[test]
    fn claude_confirm_accepts_newly_active_session_or_moved_reset() {
        let baseline = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let same = baseline.clone();
        let moved = (chrono::Utc::now() + chrono::Duration::hours(5)).to_rfc3339();
        let past = "2000-01-01T00:00:00Z";

        // Signal 1 (newly active): inactive-before → active-now with a future reset confirms even when
        // the reset value hasn't changed yet — the case the old reset-must-move check missed.
        assert!(claude_reset_confirms(
            Some(&baseline),
            Some(false),
            &same,
            Some(true)
        ));
        // Unknown baseline active state also counts as "not already active".
        assert!(claude_reset_confirms(Some(&baseline), None, &same, Some(true)));

        // P0 GUARD: already-active before + unmoved reset must NOT confirm — otherwise priming a
        // still-running window would falsely report a fresh one and persist the stale reset.
        assert!(!claude_reset_confirms(
            Some(&baseline),
            Some(true),
            &same,
            Some(true)
        ));

        // Active flag only counts with a future reset (a stale flag on a past reset is not live).
        assert!(!claude_reset_confirms(None, Some(false), past, Some(true)));

        // Signal 2 (fallback): reset moved to a new future value — stands alone, even if it was
        // already active before and `is_active` is absent now.
        assert!(claude_reset_confirms(Some(&baseline), Some(true), &moved, None));
        assert!(claude_reset_confirms(None, None, &moved, Some(false))); // valid empty precheck

        // Neither signal: inactive/unknown and the reset didn't move → not confirmed.
        assert!(!claude_reset_confirms(
            Some(&baseline),
            Some(false),
            &same,
            Some(false)
        ));
        assert!(!claude_reset_confirms(Some(&baseline), None, &same, None));
    }

    #[test]
    fn claude_confirm_budget_fits_proof_window() {
        // The whole inline confirm must finish inside the scheduler's per-tick proof budget
        // (PRIME_PROOF_BUDGET_SECONDS = 150 in app_state), leaving headroom for the HTTP reads.
        const PROOF_BUDGET: u64 = 150;
        const READ_LATENCY_HEADROOM: u64 = 20;
        assert!(
            CONFIRM_TOTAL_BUDGET.as_secs() + READ_LATENCY_HEADROOM <= PROOF_BUDGET,
            "Claude confirm budget must fit the scheduler proof window"
        );
        assert!(CONFIRM_MAX_TRIES >= 2, "must poll more than once");
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

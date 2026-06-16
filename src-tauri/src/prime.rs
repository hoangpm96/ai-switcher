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
use std::path::Path;
use std::time::Duration;

/// The cheapest model that the subscription endpoint accepts for each tool.
const CLAUDE_PRIME_MODEL: &str = "claude-haiku-4-5-20251001";
const CODEX_PRIME_MODEL: &str = "gpt-5.5";
const CLAUDE_CODE_SYSTEM_PREAMBLE: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Send-retry policy (D3): on a failed send, wait then retry, up to this many attempts total.
pub const SEND_MAX_ATTEMPTS: u32 = 5;
pub const SEND_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);
/// Confirm-retry policy (D4): after a 2xx send, re-read the live window until `reset_at` moves.
pub const CONFIRM_MAX_TRIES: u32 = 5;
pub const CONFIRM_RETRY_DELAY: Duration = Duration::from_secs(30);
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
    /// The send kept failing after `SEND_MAX_ATTEMPTS`. Carries a short reason.
    FailSend { reason: String },
    /// Send was OK but the window never moved after `CONFIRM_MAX_TRIES`.
    FailUnconfirmed,
}

/// True when this account can be primed at all: subscription (OAuth) Claude/Codex only.
/// API-proxy accounts have no 5h window; Antigravity is unsupported.
pub fn is_prime_eligible(tool_id: &ToolId, has_api_provider: bool) -> bool {
    !has_api_provider && matches!(tool_id, ToolId::Claude | ToolId::Codex)
}

/// Run one full prime attempt for an account. Blocking — the caller runs it on a worker thread.
/// `sleeper` is invoked for retry delays so tests can run without real waiting. `binary` is the
/// account's resolved CLI path (claude/codex) — used to prime via the CLI (preferred) and to
/// refresh Claude's token; None falls back to the HTTP path.
pub fn prime_account(
    tool_id: &ToolId,
    config_dir: &Path,
    binary: Option<&Path>,
    mut sleeper: impl FnMut(Duration),
) -> PrimeOutcome {
    // D1 — token must be valid.
    if read_token(tool_id, config_dir, binary).is_none() {
        return PrimeOutcome::SkipNoToken;
    }

    // D2 — if the current 5h window is still in the future, the prime would land inside it.
    let before = quota::read_live_five_hour(tool_id, config_dir);
    let before_reset = before.as_ref().and_then(|w| w.reset_at.clone());
    if let Some(reset_at) = &before_reset {
        if is_future(reset_at) {
            return PrimeOutcome::Hold {
                reset_at: reset_at.clone(),
            };
        }
    }

    // D3 — send "hi", retrying on failure.
    let mut last_reason = String::new();
    let mut sent = false;
    for attempt in 1..=SEND_MAX_ATTEMPTS {
        match send_hi(tool_id, config_dir, binary) {
            Ok(()) => {
                sent = true;
                break;
            }
            Err(reason) => {
                last_reason = reason;
                if attempt < SEND_MAX_ATTEMPTS {
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

    // D4 — confirm the window moved. The provider may take a few seconds to refresh, so
    // re-read the LIVE window a few times before giving up.
    for _ in 0..CONFIRM_MAX_TRIES {
        sleeper(CONFIRM_RETRY_DELAY);
        if let Some(window) = quota::read_live_five_hour(tool_id, config_dir) {
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

fn read_token(tool_id: &ToolId, config_dir: &Path, binary: Option<&Path>) -> Option<String> {
    match tool_id {
        ToolId::Claude => quota::claude_oauth_token_fresh(config_dir, binary),
        ToolId::Codex => quota::codex_access_token_fresh(config_dir),
        ToolId::Antigravity => None,
    }
}

/// Send a minimal "hi" to open a fresh window. Prefer the account's own CLI (`claude -p` /
/// `codex exec`) — it refreshes its own token and uses the provider's exact endpoint/model, which
/// is far more robust against token expiry (the 401 the user hit) and provider changes. Fall back
/// to a direct HTTP call only when no CLI binary is configured.
fn send_hi(tool_id: &ToolId, config_dir: &Path, binary: Option<&Path>) -> Result<(), String> {
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

/// Prime by running the account's CLI non-interactively, with the account's config dir in the
/// environment so the CLI uses the right profile/token. Exit 0 = success.
fn send_hi_cli(tool_id: &ToolId, config_dir: &Path, binary: &Path) -> Result<(), String> {
    use std::process::Command;
    let mut command = Command::new(binary);
    match tool_id {
        ToolId::Claude => {
            command
                .args(["-p", "hi", "--max-turns", "1"])
                .env("CLAUDE_CONFIG_DIR", config_dir);
        }
        ToolId::Codex => {
            command
                .args(["exec", "hi"])
                .env("CODEX_HOME", config_dir);
        }
        ToolId::Antigravity => return Err("antigravity unsupported".to_string()),
    }
    // Don't inherit a TTY; discard output so a hung/chatty CLI can't block on stdin or fill a pipe
    // buffer. We only care about the exit status.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("CLI: {e}"))?;

    // Hard timeout: a CLI that hangs (network stall, auth/update prompt) must NOT hold the prime
    // worker forever — that would block the scheduler's overlap guard and skip every later prime.
    let deadline = std::time::Instant::now() + CLI_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("CLI exit {}", status.code().unwrap_or(-1)))
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

/// POST a minimal "hi" directly. Returns Ok(()) on a 2xx, Err(reason) otherwise.
fn send_hi_http(tool_id: &ToolId, config_dir: &Path) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("client: {e}"))?;

    let response = match tool_id {
        ToolId::Claude => {
            // No binary here (HTTP fallback only runs when binary is None).
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

    #[test]
    fn eligibility_excludes_api_and_antigravity() {
        assert!(is_prime_eligible(&ToolId::Claude, false));
        assert!(is_prime_eligible(&ToolId::Codex, false));
        assert!(!is_prime_eligible(&ToolId::Claude, true)); // API-proxy account
        assert!(!is_prime_eligible(&ToolId::Antigravity, false));
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
}

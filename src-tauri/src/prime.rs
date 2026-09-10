//! "Prime ngay" — send one lightweight HTTP request to a subscription account so a fresh 5-hour
//! window opens right now, on the user's demand. One bounded attempt per button press: check the
//! token, classify the current window, send once, then poll briefly to confirm the new window.
//!
//! The old auto session prime (daily scheduler, extend reminders, pmset wake daemons) was removed —
//! its background paths were the source of every /login + folder-permission incident. This module
//! deliberately has NO background retries and NEVER spawns a CLI: everything is plain HTTP with the
//! account's existing token, so it cannot rotate a token or invalidate a live `claude` session.
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
const CLAUDE_CODE_SYSTEM_PREAMBLE: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

/// Claude confirm (D4): after a 2xx send, poll the live window until it proves a freshly anchored
/// session. Two signals (preferred → fallback):
///   1. `limits[kind == "session"].is_active == true` with a future `reset_at` — the provider flips
///      this the moment the new 5h window opens, BEFORE `reset_at` settles to a new value. Fast and
///      reliable. (D2 only sends when no real window was anchored, so an active session here is the
///      one our prime request just opened.)
///   2. `reset_at` moved to a new future value vs the pre-send baseline — the original signal, kept as
///      a fallback for payloads that don't carry `is_active`.
///
/// Read FIRST, then sleep only if not yet confirmed, so the common "opened instantly" case returns in
/// one read instead of after a fixed delay. Bounded by a max poll count AND a wall-clock budget (each
/// read is a ~1s HTTP call) so the backgrounded button press finishes in bounded time.
pub const CONFIRM_MAX_TRIES: u32 = 8;
pub const CONFIRM_RETRY_DELAY: Duration = Duration::from_secs(10);
pub const CONFIRM_TOTAL_BUDGET: Duration = Duration::from_secs(90);
/// Codex confirm (D4): after a 2xx send, poll the live window until it reads as a clearly-anchored
/// real session, tolerating a still-rolling reset or a transient read failure. Bounded by BOTH a max
/// poll count AND a hard wall-clock budget — each `read_live_five_hour` makes a `curl` call (up to
/// 20s), so counting sleeps alone undercounts.
///
/// Codex anchors a window after a tiny completed response (verified live: a completed 1% request can
/// anchor immediately), but the `reset_at` snap from rolling → fixed has a HIGHLY variable delay —
/// some sends never settle within a couple minutes. So we poll DENSELY (every 10s). If the budget
/// runs out unconfirmed, the send itself may still have anchored the window — the UI message tells
/// the user to re-check rather than claiming failure.
pub const CODEX_CONFIRM_POLL_DELAY: Duration = Duration::from_secs(10);
pub const CODEX_CONFIRM_MAX_POLLS: u32 = 12;
pub const CODEX_CONFIRM_TOTAL_BUDGET: Duration = Duration::from_secs(125);

/// The outcome of one prime attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimeOutcome {
    /// Sent + confirmed the 5h window moved to a new reset. Carries the new `reset_at` (ISO).
    Success { new_reset_at: String },
    /// The old 5h window is still active; do not prime yet. Carries that window's `reset_at`.
    Hold { reset_at: String },
    /// Account has no valid token (expired / logged out).
    SkipNoToken,
    /// Couldn't establish the current window state (read error / unparseable / inconclusive data),
    /// so we did NOT send — failing closed rather than priming into an unknown window.
    SkipUnknownState,
    /// The send failed (network / HTTP error).
    FailSend { reason: String },
    /// Send was OK but the window never confirmed within the poll budget.
    FailUnconfirmed,
}

/// True when this account can be primed at all: subscription (OAuth) Claude/Codex only.
/// API-proxy accounts have no 5h window; Antigravity is unsupported.
pub fn is_prime_eligible(tool_id: &ToolId, has_api_provider: bool) -> bool {
    !has_api_provider && matches!(tool_id, ToolId::Claude | ToolId::Codex)
}

/// Run ONE bounded prime attempt (send once, confirm with a bounded poll), emitting a one-line
/// `trace(...)` at every action (D1 token check, D2 window classification, D3 send, D4 confirm) so
/// the activity log records the full story of the attempt.
pub fn prime_account_traced(
    tool_id: &ToolId,
    config_dir: &Path,
    mut sleeper: impl FnMut(Duration),
    mut trace: impl FnMut(&str),
) -> PrimeOutcome {
    // Hold the Mac awake for the whole attempt (the confirm poll can run ~2 minutes) so an
    // idle-sleep can't cut it off. Dropped at function exit (any return path). No-op when
    // caffeinate is missing (non-macOS / stripped).
    let _awake = CaffeinateGuard::start();

    // D1 — token must be present AND unexpired. The app NEVER refreshes a Claude token itself —
    // neither directly nor by spawning the `claude` CLI to do it. Anthropic's grant rotates the
    // one-time-use refresh token, and ANY rotation (app-side or CLI-side) invalidates the chain a
    // live/overnight `claude` session still holds → that session's next refresh 401s → "/login" (the
    // exact pain this app exists to remove). Verified live 2026-07-08/09: even the CLI's own
    // background refresh is NOT multi-session-safe. So an expired token is always a SkipNoToken:
    // the user opens that account's `claude` CLI themselves (interactively, the one thing proven
    // safe) and presses the button again.
    if matches!(tool_id, ToolId::Claude) {
        trace("D1 kiểm tra token Claude");
        let exp_hhmm = quota::claude_token_expiry_hhmm(config_dir);
        match quota::claude_token_state(config_dir) {
            quota::ClaudeTokenState::Valid => trace(&format!(
                "D1 token còn hạn (tới {exp_hhmm}) → prime thẳng qua HTTP"
            )),
            quota::ClaudeTokenState::Missing => {
                trace("D1 không đọc được token (chưa đăng nhập) → SkipNoToken");
                return PrimeOutcome::SkipNoToken;
            }
            quota::ClaudeTokenState::Expired => {
                trace(&format!(
                    "D1 token đã hết hạn ({exp_hhmm}) → SkipNoToken (KHÔNG tự làm mới — mở `claude` trên account này để đăng nhập lại rồi bấm lại)"
                ));
                return PrimeOutcome::SkipNoToken;
            }
        }
    } else {
        trace("D1 kiểm tra token");
        if read_token(tool_id, config_dir).is_none() {
            trace("D1 không có token → SkipNoToken");
            return PrimeOutcome::SkipNoToken;
        }
    }

    // D2 — if a REAL 5h window is still running, the prime would land inside it → HOLD.
    // Single-snapshot classification (no probe): sending a tiny request is as cheap and harmless as typing it
    // in the terminal, so we only HOLD when the window is DEFINITELY a real anchored one. Codex's
    // `Ambiguous` (reset ≈ now + 5h — rolling for a low-usage account, which never anchors from a
    // bare prompt) falls through to send, matching the terminal's behaviour.
    trace("D2 đọc trạng thái window hiện tại");
    let before = match quota::read_live_five_hour(tool_id, config_dir) {
        Ok(window) => window,
        Err(_) => {
            trace("D2 đọc window lỗi → SkipUnknownState");
            return PrimeOutcome::SkipUnknownState;
        }
    };
    let before_reset = before.reset_at.clone();
    let before_active = before.is_active;
    let state = quota::classify_five_hour(tool_id, &before);
    match state {
        // A real anchored window is running → don't pile a prime into it. `Anchored` is only
        // produced from a parseable future reset, so `before_reset` is present.
        quota::WindowState::Anchored => match &before_reset {
            Some(reset_at) => {
                trace(&format!(
                    "D2 window đang chạy (anchored, reset {reset_at}) → HOÃN"
                ));
                return PrimeOutcome::Hold {
                    reset_at: reset_at.clone(),
                };
            }
            None => {
                trace("D2 anchored nhưng thiếu reset → SkipUnknownState");
                return PrimeOutcome::SkipUnknownState;
            }
        },
        // We can't establish the window state → fail CLOSED: do NOT send blindly.
        quota::WindowState::Unknown => {
            trace("D2 không xác định được window → SkipUnknownState");
            return PrimeOutcome::SkipUnknownState;
        }
        // Primeable (ended/no window) or Ambiguous (Codex rolling) → send.
        quota::WindowState::Primeable => trace("D2 window đã hết/chưa có → gửi prime request"),
        quota::WindowState::Ambiguous => trace("D2 window rolling (Codex) → gửi prime request"),
    }

    // D3 — send the prime request ONCE. No retry burst: this is a user-triggered button; a failure
    // returns quickly and the user can simply tap again.
    trace("D3 gửi prime request");
    if let Err(reason) = send_hi_http(tool_id, config_dir) {
        trace(&format!("D3 gửi prime request lỗi: {reason}"));
        return PrimeOutcome::FailSend { reason };
    }
    trace("D3 gửi prime request OK (HTTP 2xx + body hoàn tất)");

    // D4 — confirm the prime took.
    //
    // Provider-aware:
    //   - Codex: a completed lightweight response anchors the 5h window, but the anchor lands with a
    //     variable delay: the `/wham/usage` reset stays "rolling" (≈ now + 5h, advancing with wall
    //     time) for a moment, then snaps to a FIXED epoch that counts down. We confirm by POLLING the
    //     window until it reads as `Anchored` (reset clearly inside now+5h, i.e. not the rolling
    //     signature). This is robust to both a slow anchor and a transient read failure — a failed
    //     read just retries on the next poll instead of aborting the whole confirmation.
    //   - Claude: `reset_at` is a stable anchor, so we confirm the window actually moved to a new
    //     future reset before claiming success. Poll a few times — the provider may take a few
    //     seconds to refresh.
    trace("D4 bắt đầu xác nhận window mới");
    if matches!(tool_id, ToolId::Codex) {
        return match codex_confirm_anchored(config_dir, &mut sleeper, &mut trace) {
            Some(new_reset_at) => {
                trace(&format!("D4 xác nhận OK, reset mới {new_reset_at}"));
                PrimeOutcome::Success { new_reset_at }
            }
            None => {
                trace("D4 hết budget chưa xác nhận → FailUnconfirmed");
                PrimeOutcome::FailUnconfirmed
            }
        };
    }
    match claude_confirm_anchored(
        config_dir,
        before_reset.as_deref(),
        before_active,
        &mut sleeper,
        &mut trace,
    ) {
        Some(new_reset_at) => {
            trace(&format!("D4 xác nhận OK, reset mới {new_reset_at}"));
            PrimeOutcome::Success { new_reset_at }
        }
        None => {
            trace("D4 hết budget chưa xác nhận → FailUnconfirmed");
            PrimeOutcome::FailUnconfirmed
        }
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
    mut trace: impl FnMut(&str),
) -> Option<String> {
    // One summary line at the end, not one per poll (Claude usually anchors in 1–2 polls, but a
    // failing burst shouldn't spam the log either).
    let started = std::time::Instant::now();
    let mut polls_done = 0u32;
    for poll in 0..CONFIRM_MAX_TRIES {
        if poll > 0 {
            // Stop before a sleep that would push us past the wall-clock budget (each read below also
            // makes an HTTP call), then take one final read after the loop is no longer worth sleeping.
            if started.elapsed() + CONFIRM_RETRY_DELAY >= CONFIRM_TOTAL_BUDGET {
                break;
            }
            sleeper(CONFIRM_RETRY_DELAY);
        }
        polls_done += 1;
        if let Some(reset_at) =
            claude_anchored_reset(config_dir, baseline_reset_at, baseline_active)
        {
            trace(&format!(
                "D4 window đã neo sau {} lần đọc / {}s",
                polls_done,
                started.elapsed().as_secs()
            ));
            return Some(reset_at);
        }
        if started.elapsed() >= CONFIRM_TOTAL_BUDGET {
            break;
        }
    }
    trace(&format!(
        "D4 window chưa neo sau {} lần đọc / {}s (reset chưa đổi / chưa active)",
        polls_done,
        started.elapsed().as_secs()
    ));
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
    claude_reset_confirms(
        baseline_reset_at,
        baseline_active,
        &reset_at,
        window.is_active,
    )
    .then_some(reset_at)
}

/// Whether a Claude window read proves a session that THIS prime freshly opened.
///
/// Signal 1 — newly active: the session is now active with a future reset AND it was NOT already
/// active before we sent (`baseline_active != Some(true)`). The transition is what proves our prime
/// request opened the window; an already-active baseline with an unmoved reset must NOT count, or
/// clicking "Prime now" on a still-running window would falsely report a fresh window and persist the
/// old reset. (D2 already HOLDs a clearly-anchored window upstream; this is defense in depth so the
/// predicate is correct regardless of caller.)
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
/// count and a hard wall-clock budget (each read costs up to a 20s curl).
fn codex_confirm_anchored(
    config_dir: &Path,
    mut sleeper: impl FnMut(Duration),
    mut trace: impl FnMut(&str),
) -> Option<String> {
    // Consolidated logging: one summary line at the end, not two per poll — a full rolling burst is
    // 12 polls, and the old per-poll "đọc window"/"vẫn rolling" pair flooded the log.
    let started = std::time::Instant::now();
    let mut previous_epoch: Option<i64> = None;
    let mut polls_done = 0u32;
    let mut read_errors = 0u32;
    for poll in 0..CODEX_CONFIRM_MAX_POLLS {
        if poll > 0 {
            // Stop before a sleep that would push us past the wall-clock budget. Each read below also
            // costs up to a 20s curl, so the count cap alone is not enough to bound real elapsed time.
            if started.elapsed() + CODEX_CONFIRM_POLL_DELAY >= CODEX_CONFIRM_TOTAL_BUDGET {
                break;
            }
            sleeper(CODEX_CONFIRM_POLL_DELAY);
        }
        polls_done += 1;
        if let Ok(window) = quota::read_live_five_hour(&ToolId::Codex, config_dir) {
            // Signal 1: clearly anchored (reset far from now+5h).
            if matches!(
                quota::classify_five_hour(&ToolId::Codex, &window),
                quota::WindowState::Anchored
            ) {
                // `Anchored` is only produced from a parseable future reset, so this is present.
                if let Some(reset_at) = window.reset_at {
                    trace(&format!(
                        "D4 Codex neo rõ (anchored) sau {} poll / {}s",
                        polls_done,
                        started.elapsed().as_secs()
                    ));
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
                    trace(&format!(
                        "D4 Codex reset cố định 2 lần đọc → đã neo (sau {} poll / {}s)",
                        polls_done,
                        started.elapsed().as_secs()
                    ));
                    return window.reset_at;
                }
                previous_epoch = Some(reset_epoch);
            }
        } else {
            read_errors += 1;
        }
        if started.elapsed() >= CODEX_CONFIRM_TOTAL_BUDGET {
            break;
        }
    }
    let err_note = if read_errors > 0 {
        format!(", {read_errors} lần đọc lỗi tạm")
    } else {
        String::new()
    };
    trace(&format!(
        "D4 Codex window vẫn rolling sau {} poll / {}s{} → chưa neo",
        polls_done,
        started.elapsed().as_secs(),
        err_note
    ));
    None
}

/// Parse a Codex `reset_at` (RFC3339) to a unix epoch, but only if it is in the future. A past or
/// unparseable reset is not a live window to confirm against.
fn codex_reset_epoch_if_future(reset_at: &str) -> Option<i64> {
    let reset = chrono::DateTime::parse_from_rfc3339(reset_at).ok()?;
    (reset > chrono::Utc::now()).then(|| reset.timestamp())
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
        // Priming opens a fresh 5-hour window, which only Claude and Codex have.
        ToolId::Cursor | ToolId::Opencode | ToolId::Antigravity => None,
    }
}

/// POST a lightweight request directly. Returns Ok(()) only after the HTTP status is 2xx AND the
/// response body has been fully consumed.
///
/// This matters for Codex because `/backend-api/codex/responses` is SSE. A 2xx response means the
/// response stream was created; dropping it immediately can cancel the tiny turn before provider
/// accounting turns it into a real anchored window. Draining the body mirrors a normal completed CLI
/// turn and avoids false "sent OK" entries that never move `reset_at`.
fn send_hi_http(tool_id: &ToolId, config_dir: &Path) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| format!("client: {e}"))?;

    let response = match tool_id {
        ToolId::Claude => {
            // Reached only on the VALID-token fast path (D1 classified the token as usable), so this
            // is a plain read of the current (still-valid) access token — no refresh.
            let token =
                quota::claude_oauth_token_fresh(config_dir).ok_or_else(|| "token".to_string())?;
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
            let body = codex_prime_body();
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
        other => return Err(format!("{} unsupported", other.as_str())),
    };

    match response {
        Ok(resp) => consume_prime_response(resp),
        Err(e) if e.is_timeout() => Err("timeout".to_string()),
        Err(e) => Err(format!("network: {e}")),
    }
}

fn codex_prime_body() -> serde_json::Value {
    json!({
        "model": CODEX_PRIME_MODEL,
        "instructions": "Reply exactly OK. Do not inspect files or use tools.",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": "Open a lightweight Codex session window. Reply exactly OK."}],
        }],
        "store": false,
        "stream": true,
    })
}

fn consume_prime_response(resp: reqwest::blocking::Response) -> Result<(), String> {
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    resp.bytes().map(|_| ()).map_err(|e| {
        if e.is_timeout() {
            "body timeout".to_string()
        } else {
            format!("body: {e}")
        }
    })
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
    fn codex_prime_body_is_streamed_and_brief() {
        let body = codex_prime_body();
        assert_eq!(
            body.get("model").and_then(serde_json::Value::as_str),
            Some(CODEX_PRIME_MODEL)
        );
        assert_eq!(
            body.get("stream").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            body.get("store").and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert!(
            body.get("instructions")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|text| text.contains("Reply exactly OK")),
            "prime should request a short completion so draining SSE stays cheap"
        );
        let prompt = body
            .get("input")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("content"))
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        assert!(prompt.contains("Reply exactly OK"));
        assert!(prompt.len() < 120);
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
        assert!(claude_reset_confirms(
            Some(&baseline),
            None,
            &same,
            Some(true)
        ));

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
        assert!(claude_reset_confirms(
            Some(&baseline),
            Some(true),
            &moved,
            None
        ));
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
}

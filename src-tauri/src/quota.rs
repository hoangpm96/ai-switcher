use crate::models::{QuotaInfo, QuotaWindow, ToolId};
use crate::tools::home_dir;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Treat a token as needing renewal once it is within this skew of `expiresAt`, so a prime never
/// sends with a token that expires mid-flight (the exact failure mode of the 2026-06-25 morning
/// primes: token expired during the confirm polls). 10 minutes comfortably covers one prime burst
/// (send + up to 45' of scheduler retries) with margin.
const CLAUDE_TOKEN_EXPIRY_SKEW_MS: i64 = 10 * 60 * 1000;

/// Claude invocation used for background OAuth refreshes and primes.
///
/// `--safe-mode` disables customisations, but Claude 2.1.x can still initialise its built-in
/// tool/sandbox layer and preflight macOS protected folders. Disabling every tool and context
/// source keeps this an API-only request while preserving OAuth/Keychain refresh behaviour.
pub(crate) const CLAUDE_BACKGROUND_ARGS: &[&str] = &[
    "-p",
    "hi",
    "--max-turns",
    "1",
    "--no-session-persistence",
    "--safe-mode",
    "--setting-sources",
    "",
    "--strict-mcp-config",
    "--mcp-config",
    "{\"mcpServers\":{}}",
    "--tools",
    "",
    "--disable-slash-commands",
    "--no-chrome",
    "--permission-mode",
    "dontAsk",
    "--prompt-suggestions",
    "false",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiveQuotaError {
    RateLimited,
    Authentication,
    Network,
    InvalidResponse,
    Unsupported,
}

fn classify_live_quota_error(error: &anyhow::Error) -> LiveQuotaError {
    let message = error.to_string().to_ascii_lowercase();
    if message.contains("429") {
        LiveQuotaError::RateLimited
    } else if message.contains("401")
        || message.contains("403")
        || message.contains("access_token")
        || message.contains("oauth token")
    {
        LiveQuotaError::Authentication
    } else if message.contains("timeout")
        || message.contains("network")
        || message.contains("connection")
    {
        LiveQuotaError::Network
    } else {
        LiveQuotaError::InvalidResponse
    }
}

/// `config_dir` is the `CLAUDE_CONFIG_DIR` of the account being read (profile dir,
/// or `~/.claude` for the default account). Claude stores the keychain token by the
/// hash of this path, so the correct dir must be passed to read each account's quota.
pub fn read_quota(tool_id: &ToolId, config_dir: &Path) -> QuotaInfo {
    // While the Mac is awake (this UI/periodic read only runs then), mirror an app-managed DIR
    // account's keychain token into its `.credentials.json` file if the file is missing/stale. The
    // file is what an unattended DarkWake prime can read (the keychain is locked then). This only
    // COPIES the existing token — it never refreshes/rotates — so it respects the read-only UI policy.
    if matches!(tool_id, ToolId::Claude) {
        seed_claude_credentials_file(config_dir);
    }

    let result = match tool_id {
        ToolId::Codex => read_codex_quota(config_dir),
        ToolId::Claude => read_claude_quota(config_dir),
        ToolId::Antigravity => read_antigravity_quota(),
    };

    let mut quota = result.unwrap_or_else(|e| match tool_id {
        // Antigravity only exposes quota while the IDE is open (language server runs locally).
        ToolId::Antigravity => QuotaInfo::with_message("Open Antigravity IDE to read quota"),
        _ => QuotaInfo::with_message(format!("Couldn't read quota: {e:#}")),
    });
    // Tell the UI whether "Prime ngay" should be offered for this account. Computed centrally
    // here (one place, one clock read) rather than in each endpoint parser.
    quota.prime_available = prime_available_for(tool_id, &quota);
    quota
}

/// Codex's 5h windows are exactly `CODEX_FIVE_HOUR_SECONDS` long. We classify a `reset_at`
/// as "rolling" (no real window anchored yet) when it sits within `ROLLING_TOLERANCE_SECONDS`
/// of `now + CODEX_FIVE_HOUR_SECONDS` — the endpoint returns that moving value until a real
/// request anchors the window. Tolerance is wide (not a few seconds) to absorb network
/// latency, clock skew, and server-side processing.
const CODEX_FIVE_HOUR_SECONDS: i64 = 18_000;
const ROLLING_TOLERANCE_SECONDS: i64 = 90;

/// Single-snapshot classification of an account's 5h window. The two prime paths (D2 Hold, the UI
/// button) and the prime-availability flag all derive from this one function, so the rules live in
/// ONE place and fail the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowState {
    /// No real window running → priming opens a fresh one. (Window ended, or Codex reset is so far
    /// in the past it can't be a live window.)
    Primeable,
    /// A real window is running → a prime would land inside it (D2 must HOLD).
    Anchored,
    /// Codex only: future reset ≈ now + 5h. Could be ROLLING (unanchored, primeable) OR freshly
    /// anchored (a real window). A single snapshot can't tell, so callers choose their failure
    /// mode: the scheduled prime may send a cheap request, while UI labels must avoid guaranteeing
    /// that a new window opened until confirmation.
    Ambiguous,
    /// We don't actually know (read error, unparseable/missing reset with non-zero or unknown
    /// usage). Callers must fail CLOSED: hide the button, and D2 must NOT send.
    Unknown,
}

/// Classify the 5h window from a full quota snapshot. A read error → `Unknown`. See `WindowState`.
pub(crate) fn classify_window(tool_id: &ToolId, quota: &QuotaInfo) -> WindowState {
    if quota.error.is_some() {
        return WindowState::Unknown;
    }
    classify_five_hour(tool_id, &quota.five_hour)
}

/// Classify just the 5h `QuotaWindow` (used by the prime path, which reads only the live window).
/// Callers that have a full `QuotaInfo` should use `classify_window` so a read error maps to
/// `Unknown` first.
pub(crate) fn classify_five_hour(tool_id: &ToolId, window: &QuotaWindow) -> WindowState {
    if matches!(tool_id, ToolId::Antigravity) {
        return WindowState::Unknown; // can't prime
    }
    let now = chrono::Utc::now().timestamp();
    match window.reset_at.as_deref() {
        // Claude's successful usage response uses `resets_at: null` when there is no active 5-hour
        // window. `utilization` can still be non-zero because it is usage metadata, not proof that
        // a session is currently anchored. Requiring exactly 0 hid "Prime ngay" for valid,
        // logged-in accounts with no session.
        //
        // Codex is kept conservative: a missing reset is only known-primeable when the endpoint
        // explicitly reports an empty window. Full-response errors are filtered by
        // `classify_window`; `read_live_five_hour` only reaches this branch after a successful API
        // response.
        None if matches!(tool_id, ToolId::Claude) => WindowState::Primeable,
        None => match window.percent_used {
            Some(p) if p <= 0.0 => WindowState::Primeable,
            _ => WindowState::Unknown,
        },
        Some(reset_at) => {
            // An UNPARSEABLE timestamp is Unknown, never "ended".
            let Some(reset) = parse_rfc3339_epoch(reset_at) else {
                return WindowState::Unknown;
            };
            if reset <= now {
                return WindowState::Primeable; // ended
            }
            match tool_id {
                // Claude's future reset_at is always a real anchored window.
                ToolId::Claude => WindowState::Anchored,
                // Codex future reset near now+5h is ambiguous (rolling vs fresh anchor); farther
                // from now+5h is a clearly-anchored real window.
                ToolId::Codex => {
                    if reset_is_near_full_window(reset, now) {
                        WindowState::Ambiguous
                    } else {
                        WindowState::Anchored
                    }
                }
                ToolId::Antigravity => WindowState::Unknown,
            }
        }
    }
}

/// Whether a Codex reset sits within tolerance of `now + 5h` — the single-snapshot signature shared
/// by both a rolling (unanchored) window and one anchored only seconds ago.
fn reset_is_near_full_window(reset: i64, now: i64) -> bool {
    (reset - now - CODEX_FIVE_HOUR_SECONDS).abs() <= ROLLING_TOLERANCE_SECONDS
}

/// Whether the user can open a fresh 5h window right now. See `QuotaInfo::prime_available`.
/// Single-snapshot UI heuristic: `Ambiguous` is shown as available, but the click result still comes
/// from the prime path's post-send confirmation, so the UI must phrase it as a request unless the
/// backend returns confirmed success.
fn prime_available_for(tool_id: &ToolId, quota: &QuotaInfo) -> Option<bool> {
    match classify_window(tool_id, quota) {
        WindowState::Primeable | WindowState::Ambiguous => Some(true),
        WindowState::Anchored => Some(false),
        WindowState::Unknown => None,
    }
}

/// `reset_at` (ISO 8601) parsed to a unix timestamp, or None if unparseable.
fn parse_rfc3339_epoch(reset_at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(reset_at)
        .ok()
        .map(|t| t.timestamp())
}

// ---------------------------------------------------------------------------
// Claude Code — calls the OAuth usage endpoint (same source the /usage command uses).
//
// The OAuth token lives in the macOS Keychain. Since Claude Code 2.x, each config dir has
// its own keychain entry keyed by the path hash:
//   service = "Claude Code-credentials-<sha256(CLAUDE_CONFIG_DIR)[:8]>"
// (it no longer writes the file ~/.claude/.credentials.json). This way each profile has its
// own separate credential — reading the right dir reads the right account.
//
// Endpoint:
//   GET https://api.anthropic.com/api/oauth/usage
//   Authorization: Bearer <accessToken>
//   anthropic-beta: oauth-2025-04-20
//   User-Agent: claude-code/<version>   (missing this header causes repeated 429s)
// Returns five_hour.utilization / seven_day.utilization (0–100) + resets_at ISO.
//
// The endpoint rate-limit is fairly strict, so cache the result for 60s PER config dir (using
// a single shared cache would let one account's quota mask another's when refreshing many accounts).
// ---------------------------------------------------------------------------

static CLAUDE_CACHE: Mutex<BTreeMap<String, (Instant, QuotaInfo)>> = Mutex::new(BTreeMap::new());
const CLAUDE_CACHE_TTL: Duration = Duration::from_secs(60);

/// Drop the cached Claude quota for one config dir so the next `read_quota` re-fetches.
/// Auto-prime's confirmation re-check needs the fresh `resets_at` right after sending "hi",
/// which the 60s cache would otherwise mask.
pub(crate) fn invalidate_claude_cache(config_dir: &Path) {
    if let Ok(mut guard) = CLAUDE_CACHE.lock() {
        guard.remove(&config_dir.to_string_lossy().to_string());
    }
}

fn read_claude_quota(config_dir: &Path) -> Result<QuotaInfo> {
    let cache_key = config_dir.to_string_lossy().to_string();
    if let Ok(guard) = CLAUDE_CACHE.lock() {
        if let Some((fetched_at, quota)) = guard.get(&cache_key) {
            if fetched_at.elapsed() < CLAUDE_CACHE_TTL {
                return Ok(quota.clone());
            }
        }
    }

    let token = claude_oauth_token_fresh(config_dir)
        .context("couldn't get Claude's OAuth token")?;
    let version = claude_version().unwrap_or_else(|| "0.0.0".to_string());
    let user_agent = format!("claude-code/{version}");
    let request = |token: &str| {
        curl_get(
            "https://api.anthropic.com/api/oauth/usage",
            &[
                ("Authorization", format!("Bearer {token}").as_str()),
                ("anthropic-beta", "oauth-2025-04-20"),
                ("User-Agent", user_agent.as_str()),
                ("Accept", "application/json"),
            ],
        )
    };
    // A 401/429 here means the stored access token has expired. We deliberately do NOT refresh it
    // ourselves — rotating the one-time-use refresh token would invalidate a live `claude` session on
    // this account (see `claude_oauth_token_fresh`). Surface the error so the UI shows an "open Claude
    // Code to refresh" hint; the token gets renewed, conflict-free, the next time the CLI runs.
    let body = request(&token)?;

    let value: serde_json::Value =
        serde_json::from_str(&body).context("Claude usage response is not JSON")?;
    let quota = quota_from_claude_usage(&value)?;

    if let Ok(mut guard) = CLAUDE_CACHE.lock() {
        guard.insert(cache_key, (Instant::now(), quota.clone()));
    }
    Ok(quota)
}

fn quota_from_claude_usage(value: &serde_json::Value) -> Result<QuotaInfo> {
    let session_active = claude_session_is_active(value);
    let five_hour = claude_window("5-hour limit", value.get("five_hour"), session_active);
    let weekly = claude_window("Weekly limit", value.get("seven_day"), None);

    if five_hour.percent_used.is_none() && weekly.percent_used.is_none() {
        anyhow::bail!("Claude usage has no utilization");
    }

    Ok(QuotaInfo {
        five_hour,
        weekly,
        models: None,
        plan: claude_plan(value),
        // Overwritten centrally by `read_quota` via `prime_available_for`.
        prime_available: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    })
}

/// Best-effort plan label from Claude's usage payload. The endpoint isn't documented to
/// always carry one, so try a few likely keys and ignore if absent.
fn claude_plan(value: &serde_json::Value) -> Option<String> {
    for key in [
        "subscription_type",
        "plan",
        "plan_type",
        "tier",
        "account_type",
    ] {
        if let Some(raw) = value.get(key).and_then(serde_json::Value::as_str) {
            if let Some(plan) = pretty_plan(raw) {
                return Some(plan);
            }
        }
    }
    None
}

fn claude_window(
    label: &str,
    value: Option<&serde_json::Value>,
    is_active: Option<bool>,
) -> QuotaWindow {
    let percent_used = value
        .and_then(|w| w.get("utilization"))
        .and_then(serde_json::Value::as_f64);
    let reset_at = value
        .and_then(|w| w.get("resets_at"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
    QuotaWindow {
        label: label.to_string(),
        percent_used,
        reset_at,
        is_active,
    }
}

/// `limits[]` carries one entry per window kind, each with its own `is_active`. The 5h window is the
/// `kind == "session"` entry; its `is_active` flips to `true` the moment a fresh window opens (before
/// `five_hour.resets_at` propagates a new value), which is the signal the prime confirm polls for.
fn claude_session_is_active(value: &serde_json::Value) -> Option<bool> {
    value
        .get("limits")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .find(|entry| entry.get("kind").and_then(serde_json::Value::as_str) == Some("session"))?
        .get("is_active")
        .and_then(serde_json::Value::as_bool)
}

pub(crate) fn claude_oauth_token(config_dir: &Path) -> Option<String> {
    let raw = claude_credentials_blob(config_dir)?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("accessToken"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

/// Returns the account's current Claude access token WITHOUT refreshing it.
///
/// This is the READ-ONLY accessor used by the UI quota path. It deliberately never refreshes the
/// token: rotating Anthropic's one-time-use refresh token would consume the token a live `claude`
/// CLI session (on the same account) still holds, so its next request would 401 with "Please run
/// /login". During normal daytime use a CLI session may well be live, so the UI tolerates a stale
/// token (surfacing an "open Claude Code to refresh" hint) rather than risk clobbering it.
///
/// When a token is expired, renewal is delegated to the `claude` CLI itself (see
/// `claude_token_state` / prime D1) — the app never runs the refresh grant.
///
/// (Kept as a distinct name from `claude_oauth_token` so call sites read intentionally; both now do
/// the same read-only thing.)
pub(crate) fn claude_oauth_token_fresh(config_dir: &Path) -> Option<String> {
    claude_oauth_token(config_dir)
}

/// Offline classification of an account's stored Claude credential. The app NEVER refreshes or
/// rotates a Claude token itself — Anthropic's refresh grant rotates the one-time-use refresh token,
/// and any app-side rotation invalidates the chain a live/overnight `claude` session still holds,
/// forcing "/login" (the exact failure the app exists to avoid). When a token is `Expired`, the
/// prime path hands the WHOLE job (refresh + "hi") to the `claude` CLI, whose own refresh mechanism
/// is what concurrent CLI sessions already share safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClaudeTokenState {
    /// No credential stored (logged out / never logged in).
    Missing,
    /// Access token valid for at least `CLAUDE_TOKEN_EXPIRY_SKEW_MS` more — usable as-is.
    Valid,
    /// Present but expired (or about to): only the CLI may renew it.
    Expired,
}

/// Classify the stored Claude credential OFFLINE (reads `claudeAiOauth.expiresAt`, no network).
pub(crate) fn claude_token_state(config_dir: &Path) -> ClaudeTokenState {
    let Some(raw) = claude_credentials_blob(config_dir) else {
        return ClaudeTokenState::Missing;
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(value) if claude_token_still_valid(&value) => ClaudeTokenState::Valid,
        Ok(_) => ClaudeTokenState::Expired,
        Err(_) => ClaudeTokenState::Missing,
    }
}

/// The stored token's `expiresAt` (epoch ms), offline. `None` when no credential / no `expiresAt`.
pub(crate) fn claude_token_expiry_ms(config_dir: &Path) -> Option<i64> {
    claude_credentials_blob(config_dir)
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("claudeAiOauth")
                .and_then(|o| o.get("expiresAt"))
                .and_then(serde_json::Value::as_i64)
        })
}

/// The stored token's `expiresAt` as a local `HH:MM` string, for human-readable log lines (offline).
/// Returns "?" when no credential / no `expiresAt`.
pub(crate) fn claude_token_expiry_hhmm(config_dir: &Path) -> String {
    let Some(ms) = claude_token_expiry_ms(config_dir) else {
        return "?".to_string();
    };
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "?".to_string())
}

/// Whether the account's per-dir keychain item is READABLE right now. Returns false when the login
/// keychain is locked — which is exactly the DarkWake state (Mac woken in the background before any
/// GUI login). The prime path uses this as a proxy for "awake enough to renew": renewing a Claude
/// token means running the CLI, which reads/writes the keychain; doing that while locked either fails
/// or lands the fresh token in a still-locked keychain that the confirm read can't see. So when this
/// is false the prime DEFERS the CLI renewal to a later (awake) tick instead of failing. A DIR
/// account only; `~/.claude` always reports readable (its own CLI owns that lifecycle).
pub(crate) fn claude_keychain_readable(config_dir: &Path) -> bool {
    if config_dir == home_dir().join(".claude") {
        return true;
    }
    let suffix = claude_keychain_suffix(config_dir);
    read_keychain_blob(&format!("Claude Code-credentials-{suffix}")).is_some()
}

/// Public wrapper: re-mirror the account's keychain token into its `.credentials.json` file. Called
/// by the prime path right after the CLI renews the token, so the fresh keychain value is copied
/// back into the DarkWake-readable file (the CLI deletes the file when it rotates). Copy-only.
pub(crate) fn reseed_claude_file(config_dir: &Path) {
    seed_claude_credentials_file(config_dir);
}

/// Mirror an app-managed DIR account's keychain token into its `.credentials.json` file, so an
/// unattended DarkWake prime (when the login keychain is locked and unreadable) can still read the
/// token from the file. Best-effort and COPY-ONLY — never refreshes/rotates the token, so it is safe
/// to call from the read-only UI quota path. No-op for the default `~/.claude` account (left to the
/// CLI's own keychain lifecycle) and when the keychain isn't currently readable.
///
/// Keeps the file fresh by `expiresAt`: the keychain blob is written over the file ONLY when it is
/// strictly newer (e.g. the user ran `claude` and the CLI rotated the keychain) or the file is
/// missing. It never overwrites a fresher file (an app file-only refresh can leave the keychain
/// older — copying that back would strand the account with a dead refresh token).
fn seed_claude_credentials_file(config_dir: &Path) {
    if config_dir == home_dir().join(".claude") {
        return; // default account: keychain-only, never mirror
    }
    let suffix = claude_keychain_suffix(config_dir);
    let Some(keychain_blob) = read_keychain_blob(&format!("Claude Code-credentials-{suffix}")) else {
        return; // keychain locked/empty (DarkWake) or account uses file already — nothing to mirror
    };
    let file = config_dir.join(".credentials.json");
    let file_blob = read_file_blob(&file);
    // Identical → nothing to do (avoids disk churn every read).
    if file_blob.as_deref() == Some(keychain_blob.as_str()) {
        return;
    }
    // Only copy keychain→file when the keychain token is STRICTLY NEWER (or the file is missing /
    // unparseable) — the newest token stays authoritative. The CLI writes its refreshes to the
    // keychain, so keychain-newer is the normal direction; guarding by `expiresAt` just makes the
    // copy safe no matter which store happens to be fresher.
    if let Some(existing) = &file_blob {
        if blob_expires_at(existing) >= blob_expires_at(&keychain_blob) {
            return; // file is as-new-or-newer than keychain → keep the file
        }
    }
    let _ = write_file_credentials(&file, &keychain_blob);
}

/// Parse `claudeAiOauth.expiresAt` (epoch ms) from a credential blob, or 0 if absent/unparseable.
/// Used to decide which of two stored blobs is newer.
fn blob_expires_at(blob: &str) -> i64 {
    serde_json::from_str::<serde_json::Value>(blob)
        .ok()
        .and_then(|v| {
            v.get("claudeAiOauth")
                .and_then(|o| o.get("expiresAt"))
                .and_then(serde_json::Value::as_i64)
        })
        .unwrap_or(0)
}

/// True when the blob's `claudeAiOauth.expiresAt` (epoch ms) is far enough in the future to use for
/// a full prime burst. A missing/zero `expiresAt` is treated as needing renewal (fail safe).
fn claude_token_still_valid(value: &serde_json::Value) -> bool {
    value
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("expiresAt"))
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|expiry| expiry > chrono::Utc::now().timestamp_millis() + CLAUDE_TOKEN_EXPIRY_SKEW_MS)
}

/// Atomically write a credential blob to a `.credentials.json` file (temp-write + rename), with
/// owner-only `0600` perms. Preserves existing perms on an overwrite; sets `0600` on a fresh file so
/// a seeded token isn't world-readable. Used by the keychain→file seed — the app's ONLY credential
/// write; it never writes the keychain (no rotation, no keychain password prompts).
fn write_file_credentials(path: &Path, blob: &str) -> bool {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let existing_perms = std::fs::metadata(path).ok().map(|meta| meta.permissions());
    let temporary = path.with_extension("json.tmp");
    // Create the temp file with 0600 from the start (mode applied at open, before any bytes land) so
    // the OAuth token is never briefly world-readable. mode() is masked by umask but 0600 has no group
    // /other bits to mask, so the result is owner-only regardless of umask.
    let opened = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary);
    let Ok(mut f) = opened else {
        return false;
    };
    if f.write_all(blob.as_bytes()).is_err() || f.sync_all().is_err() {
        let _ = std::fs::remove_file(&temporary);
        return false;
    }
    drop(f);
    if std::fs::rename(&temporary, path).is_err() {
        let _ = std::fs::remove_file(&temporary);
        return false;
    }
    // Preserve the original file's perms on an overwrite; a fresh file keeps the 0600 set above.
    if let Some(perms) = existing_perms {
        let _ = std::fs::set_permissions(path, perms);
    } else {
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    true
}

/// Claude's keychain suffix for a config dir = `sha256(path)[:8]` (hex).
pub fn claude_keychain_suffix(config_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(config_dir.to_string_lossy().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Gets Claude's JSON credential blob for a specific config dir. Read-only — we never write the
/// keychain back (the OAuth lifecycle stays entirely with Claude Code; see `claude_oauth_token_fresh`).
///
/// Order: (1) per-dir keychain by hash (Claude 2.x) → (2) the `.credentials.json` file inside the
/// config dir itself (older versions stored a per-dir file) → (3) ONLY when it is the default dir
/// `~/.claude`: try the old global keychain name. Do NOT fall back to the global one for a profile
/// dir, to avoid reading the wrong account's token.
fn claude_credentials_blob(config_dir: &Path) -> Option<String> {
    // For an app-managed DIR account, read the `.credentials.json` FILE first. The keychain item is
    // unreadable while the Mac is in DarkWake (woken in the background for a scheduled prime, before
    // any GUI login unlocks the login keychain) — `security` returns empty, which read as "no token"
    // and failed the morning primes. The file has no such lock, so seeding the token into it (see
    // `seed_claude_credentials_file`) lets a DarkWake prime read it. Falls back to keychain if no file
    // yet. The default `~/.claude` account is left keychain-first — it's the user's own CLI profile,
    // which owns its keychain lifecycle and is never primed unattended in the background.
    let is_dir_account = config_dir != home_dir().join(".claude");
    let suffix = claude_keychain_suffix(config_dir);
    if is_dir_account {
        if let Some(blob) = read_file_blob(&config_dir.join(".credentials.json")) {
            return Some(blob);
        }
    }
    if let Some(blob) = read_keychain_blob(&format!("Claude Code-credentials-{suffix}")) {
        return Some(blob);
    }
    if !is_dir_account {
        if let Some(blob) = read_file_blob(&config_dir.join(".credentials.json")) {
            return Some(blob);
        }
        if let Some(blob) = read_keychain_blob("Claude Code-credentials") {
            return Some(blob);
        }
    }
    None
}

fn read_keychain_blob(service: &str) -> Option<String> {
    Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|blob| !blob.is_empty())
}

fn read_file_blob(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .filter(|text| !text.trim().is_empty())
}

pub(crate) fn claude_version() -> Option<String> {
    let path = crate::tools::command_path("claude")?;
    let output = Command::new(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // format "2.1.158 (Claude Code)" — take the first numeric token.
    text.split_whitespace()
        .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(ToString::to_string)
}

pub(crate) fn curl_get(url: &str, headers: &[(&str, &str)]) -> Result<String> {
    let mut command = Command::new("curl");
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("20")
        .arg("-w")
        .arg("\n%{http_code}"); // append status code as last line
    for (key, value) in headers {
        command.arg("-H").arg(format!("{key}: {value}"));
    }
    command.arg(url);

    let output = command.output().context("couldn't run curl")?;
    let full = String::from_utf8_lossy(&output.stdout);
    // Split off the status code appended by -w.
    let (body, status_str) = full
        .rsplit_once('\n')
        .context("unexpected curl output format")?;
    let status: u16 = status_str.trim().parse().unwrap_or(0);

    if status == 0 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("network error: {}", stderr.trim());
    }
    if status >= 400 {
        anyhow::bail!("HTTP {status}");
    }
    Ok(body.to_owned())
}

fn curl_post(url: &str, headers: &[(&str, &str)], body: &str) -> Result<String> {
    let mut command = Command::new("curl");
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--max-time")
        .arg("10")
        .arg("-X")
        .arg("POST");
    for (key, value) in headers {
        command.arg("-H").arg(format!("{key}: {value}"));
    }
    command.arg("--data").arg(body).arg(url);

    let output = command.output().context("couldn't run curl")?;
    if !output.status.success() {
        anyhow::bail!("HTTP request failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Antigravity — calls the IDE's local language-server gRPC-Web.
//
// There is no offline file with quota. While the IDE is open, each
// `language_server_*` process listens on loopback and receives `--csrf_token <token>` via args.
// We find the process (ps), get the csrf + the listening ports (lsof), then POST:
//   POST http://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus
//   x-codeium-csrf-token: <csrf>
// Response: cascadeModelConfigData.clientModelConfigs[].quotaInfo has
// `remainingFraction` (1.0 = 100% remaining) + `resetTime` (ISO). The window is 5 hours;
// Antigravity has no separate weekly window, so `weekly` is left empty.
// ---------------------------------------------------------------------------

fn read_antigravity_quota() -> Result<QuotaInfo> {
    let servers = antigravity_servers();
    if servers.is_empty() {
        anyhow::bail!("Antigravity language server not found (is the IDE open?)");
    }

    for (csrf, ports) in servers {
        for port in ports {
            let Ok(body) = antigravity_user_status(port, &csrf) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) else {
                continue;
            };
            if let Ok(quota) = quota_from_antigravity_status(&value) {
                return Ok(quota);
            }
        }
    }
    anyhow::bail!("couldn't call Antigravity's GetUserStatus")
}

/// Returns a list of (csrf_token, listening ports) for each language server.
fn antigravity_servers() -> Vec<(String, Vec<u16>)> {
    let Ok(output) = Command::new("ps")
        .args(["-ax", "-o", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);

    let mut servers = Vec::new();
    for line in text.lines() {
        let lower = line.to_lowercase();
        if !lower.contains("language_server") || !lower.contains("antigravity") {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(pid) = tokens.next() else { continue };
        let Some(csrf) = arg_value(line, "--csrf_token") else {
            continue;
        };
        let ports = listening_ports(pid);
        if !ports.is_empty() {
            servers.push((csrf, ports));
        }
    }
    servers
}

/// Gets the value following a flag in the command line (e.g. `--csrf_token <value>`).
fn arg_value(line: &str, flag: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == flag {
            return tokens.next().map(ToString::to_string);
        }
    }
    None
}

fn listening_ports(pid: &str) -> Vec<u16> {
    let Ok(output) = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-a", "-p", pid])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);

    let mut ports = Vec::new();
    for line in text.lines().skip(1) {
        if let Some(name) = line.split_whitespace().nth(8) {
            if let Some(port) = name.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
    }
    ports
}

fn antigravity_user_status(port: u16, csrf: &str) -> Result<String> {
    curl_post(
        &format!(
            "http://127.0.0.1:{port}/exa.language_server_pb.LanguageServerService/GetUserStatus"
        ),
        &[
            ("Content-Type", "application/json"),
            ("Connect-Protocol-Version", "1"),
            ("x-codeium-csrf-token", csrf),
        ],
        r#"{"metadata":{"ideName":"antigravity","extensionName":"antigravity","ideVersion":"unknown","locale":"en"}}"#,
    )
}

fn quota_from_antigravity_status(value: &serde_json::Value) -> Result<QuotaInfo> {
    let plan = value
        .get("userStatus")
        .and_then(|status| status.get("planStatus"))
        .and_then(|status| status.get("planInfo"))
        .and_then(|info| info.get("planName"))
        .and_then(serde_json::Value::as_str)
        .and_then(pretty_plan);
    let configs = value
        .get("userStatus")
        .and_then(|status| status.get("cascadeModelConfigData"))
        .and_then(|data| data.get("clientModelConfigs"))
        .and_then(serde_json::Value::as_array)
        .context("Antigravity response is missing clientModelConfigs")?;

    // One QuotaWindow per model (remainingFraction 1.0 = 0% used).
    let mut models: Vec<QuotaWindow> = Vec::new();
    for config in configs {
        let Some(quota) = config.get("quotaInfo") else {
            continue;
        };
        // Antigravity returns proto3 JSON: a field equal to its default value is OMITTED from
        // the payload. A missing `remainingFraction` = 0.0 = fully exhausted. If we skip a
        // model missing this field, the exhausted model disappears from the list and the
        // app thinks it's still full (bug: Claude is out of quota but shows 100%).
        let remaining = quota
            .get("remainingFraction")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        let label = config
            .get("label")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Model")
            .to_string();
        let reset_at = quota
            .get("resetTime")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        models.push(QuotaWindow {
            label,
            percent_used: Some(((1.0 - remaining) * 100.0).clamp(0.0, 100.0)),
            reset_at,
            is_active: None,
        });
    }

    if models.is_empty() {
        anyhow::bail!("Antigravity has no quotaInfo");
    }

    // The overall "5-hour" window = the most-used model (shared for summary/exhaustion).
    let worst = models
        .iter()
        .max_by(|a, b| {
            a.percent_used
                .unwrap_or(0.0)
                .total_cmp(&b.percent_used.unwrap_or(0.0))
        })
        .expect("models not empty");

    Ok(QuotaInfo {
        five_hour: QuotaWindow {
            label: "5-hour limit".to_string(),
            percent_used: worst.percent_used,
            reset_at: worst.reset_at.clone(),
            is_active: None,
        },
        weekly: QuotaWindow {
            label: "Weekly limit".to_string(),
            percent_used: None,
            reset_at: None,
            is_active: None,
        },
        models: Some(models),
        plan,
        // Antigravity can't prime; `prime_available_for` returns None for it anyway.
        prime_available: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    })
}

// ---------------------------------------------------------------------------
// Codex — reads the latest rate-limit snapshot from session rollout files.
//
// The Codex CLI has no `usage` command. Instead, each session stores a JSONL file at
// `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl`. Each
// `token_count` event carries `payload.rate_limits` with 2 windows:
//   - primary   → window_minutes = 300   (5 hours)
//   - secondary → window_minutes = 10080 (7 days / week)
// Each window has `used_percent` and `resets_at` (unix epoch seconds).
// We take the last rate_limits entry from the most recent rollout file that has data.
// ---------------------------------------------------------------------------

fn read_codex_quota(config_dir: &Path) -> Result<QuotaInfo> {
    // Prefer the per-account usage endpoint — it reads THIS account's live 5h window straight from
    // the provider (via the token in config_dir/auth.json), so it's current and correct no matter
    // which account last ran the CLI.
    //
    // The rollout file (~/.codex/sessions/.../rollout-*.jsonl) is only a FALLBACK now: it's shared
    // by every account (profile accounts symlink their sessions/ back to ~/.codex/sessions), so it
    // reflects whichever account last ran the CLI — not necessarily this one — and it's only updated
    // when the CLI runs, so it goes stale (it can be months old if you haven't used the CLI). Using
    // it as the primary source for the default account made "Default (máy)" show a frozen percentage
    // that Refresh never updated, while a profile account on the same login showed the live number.
    match read_codex_usage_endpoint(config_dir) {
        Ok(quota) => Ok(quota),
        Err(endpoint_err) => {
            // Endpoint failed (offline, token issue): for the default account, a (possibly stale)
            // rollout reading still beats showing nothing.
            if config_dir == home_dir().join(".codex") {
                if let Ok(quota) = read_codex_rollout_quota() {
                    return Ok(quota);
                }
            }
            Err(endpoint_err)
        }
    }
}

fn read_codex_rollout_quota() -> Result<QuotaInfo> {
    let sessions = home_dir().join(".codex/sessions");
    let limits = latest_codex_rate_limits(&sessions)
        .context("rate_limits not found in the Codex session")?;
    quota_from_codex_rate_limits(&limits)
}

/// Fallback: calls `GET https://chatgpt.com/backend-api/wham/usage` with the JWT in
/// `<config_dir>/auth.json` (`tokens.access_token`). Returns rate_limit.primary_window
/// (5h, limit_window_seconds 18000) + secondary_window (weekly, 604800), each with
/// `used_percent` + `reset_at` (unix seconds).
/// Read the LIVE 5-hour window for one account, bypassing any cache or local rollout file.
/// Auto-prime's confirmation step needs the truth straight from the provider right after
/// sending "hi" — the Claude cache (60s) or the Codex rollout file (only updated by the CLI)
/// would otherwise return a stale `reset_at`.
pub(crate) fn read_live_five_hour(
    tool_id: &ToolId,
    config_dir: &Path,
) -> std::result::Result<QuotaWindow, LiveQuotaError> {
    match tool_id {
        ToolId::Claude => {
            invalidate_claude_cache(config_dir);
            let quota = read_claude_quota(config_dir).map_err(|e| classify_live_quota_error(&e))?;
            Ok(quota.five_hour)
        }
        ToolId::Codex => {
            let quota =
                read_codex_usage_endpoint(config_dir).map_err(|e| classify_live_quota_error(&e))?;
            Ok(quota.five_hour)
        }
        ToolId::Antigravity => Err(LiveQuotaError::Unsupported),
    }
}

fn read_codex_usage_endpoint(config_dir: &Path) -> Result<QuotaInfo> {
    let token =
        codex_access_token_fresh(config_dir).context("couldn't get Codex's access_token")?;
    let body = curl_get(
        "https://chatgpt.com/backend-api/wham/usage",
        &[
            ("Authorization", format!("Bearer {token}").as_str()),
            ("Accept", "application/json"),
        ],
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&body).context("Codex usage response is not JSON")?;
    quota_from_codex_endpoint(&value)
}

/// Returns the account's current Codex access token from `auth.json`, WITHOUT refreshing it.
///
/// Same principle as `claude_oauth_token_fresh`: we never run the refresh-token grant or rewrite
/// `auth.json` ourselves. OpenAI's grant rotates the refresh token, so doing it here would consume
/// the token a live `codex` session still holds and could log that account out. The Codex CLI owns
/// the refresh lifecycle (it re-reads `auth.json` per run). Codex access tokens last ~8 days, so an
/// account essentially never goes stale between uses; if one does, its quota reads as unavailable
/// until the next `codex` run renews the token.
///
/// (Kept as a distinct name so call sites read intentionally; it is now a plain read.)
pub(crate) fn codex_access_token_fresh(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(config_dir.join("auth.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("tokens")?
        .get("access_token")?
        .as_str()
        .map(ToString::to_string)
}

pub(crate) fn codex_account_id(config_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(config_dir.join("auth.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("tokens")
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

fn quota_from_codex_endpoint(value: &serde_json::Value) -> Result<QuotaInfo> {
    let rate_limit = value
        .get("rate_limit")
        .context("Codex response is missing rate_limit")?;

    let mut five_hour = QuotaWindow {
        label: "5-hour limit".to_string(),
        ..Default::default()
    };
    let mut weekly = QuotaWindow {
        label: "Weekly limit".to_string(),
        ..Default::default()
    };

    for key in ["primary_window", "secondary_window"] {
        let Some(window) = rate_limit.get(key) else {
            continue;
        };
        if window.is_null() {
            continue;
        }
        let percent = window
            .get("used_percent")
            .and_then(serde_json::Value::as_f64);
        let reset_at = window
            .get("reset_at")
            .and_then(serde_json::Value::as_i64)
            .and_then(unix_to_rfc3339);
        let window_seconds = window
            .get("limit_window_seconds")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        // ≥ 1 day is weekly, otherwise 5-hour.
        let target = if window_seconds >= 86_400 {
            &mut weekly
        } else {
            &mut five_hour
        };
        target.percent_used = percent;
        target.reset_at = reset_at;
    }

    if five_hour.percent_used.is_none() && weekly.percent_used.is_none() {
        anyhow::bail!("Codex rate_limit is empty");
    }

    Ok(QuotaInfo {
        five_hour,
        weekly,
        models: None,
        plan: value
            .get("plan_type")
            .and_then(serde_json::Value::as_str)
            .and_then(pretty_plan),
        // Overwritten centrally by `read_quota` via `prime_available_for`.
        prime_available: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    })
}

/// Scans the rollout files (newest first) and returns the most recent `rate_limits`
/// object that still has data (primary/secondary not null).
fn latest_codex_rate_limits(sessions: &Path) -> Result<serde_json::Value> {
    let mut files = collect_jsonl_files(sessions);
    // Sort by modification time descending — the most recent file holds the newest snapshot.
    files.sort_by_key(|entry| std::cmp::Reverse(entry.1));

    // Limit the number of files read so a single refresh doesn't scan thousands of old sessions.
    for (path, _) in files.into_iter().take(40) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Some(limits) = last_rate_limits_in(&text) {
                return Ok(limits);
            }
        }
    }
    anyhow::bail!("no codex rate_limits found")
}

fn collect_jsonl_files(dir: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut out = Vec::new();
    collect_jsonl_into(dir, &mut out);
    out
}

fn collect_jsonl_into(dir: &Path, out: &mut Vec<(PathBuf, std::time::SystemTime)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_into(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            out.push((path, modified));
        }
    }
}

/// Gets the last `payload.rate_limits` (with primary or secondary not null)
/// in a rollout file.
fn last_rate_limits_in(text: &str) -> Option<serde_json::Value> {
    let mut latest = None;
    for line in text.lines() {
        // Quickly skip irrelevant lines before parsing JSON.
        if !line.contains("rate_limits") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // The snapshot lives at payload.rate_limits; some older versions put it at payload.info.rate_limits.
        let payload = value.get("payload");
        let limits = payload.and_then(|p| p.get("rate_limits")).or_else(|| {
            payload
                .and_then(|p| p.get("info"))
                .and_then(|i| i.get("rate_limits"))
        });
        if let Some(limits) = limits {
            let has_data = !limits
                .get("primary")
                .unwrap_or(&serde_json::Value::Null)
                .is_null()
                || !limits
                    .get("secondary")
                    .unwrap_or(&serde_json::Value::Null)
                    .is_null();
            if has_data {
                latest = Some(limits.clone());
            }
        }
    }
    latest
}

fn quota_from_codex_rate_limits(limits: &serde_json::Value) -> Result<QuotaInfo> {
    let mut five_hour = QuotaWindow {
        label: "5-hour limit".to_string(),
        ..Default::default()
    };
    let mut weekly = QuotaWindow {
        label: "Weekly limit".to_string(),
        ..Default::default()
    };

    for key in ["primary", "secondary"] {
        let window = limits.get(key);
        let Some(window) = window else { continue };
        if window.is_null() {
            continue;
        }

        let percent = window
            .get("used_percent")
            .and_then(serde_json::Value::as_f64);
        let reset_at = window
            .get("resets_at")
            .and_then(serde_json::Value::as_i64)
            .and_then(unix_to_rfc3339);
        let window_minutes = window
            .get("window_minutes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);

        // Classify by window length: ≥ 1 day is weekly, otherwise 5-hour.
        let target = if window_minutes >= 1440 {
            &mut weekly
        } else {
            &mut five_hour
        };
        target.percent_used = percent;
        target.reset_at = reset_at;
    }

    if five_hour.percent_used.is_none() && weekly.percent_used.is_none() {
        anyhow::bail!("codex rate_limits is empty");
    }

    Ok(QuotaInfo {
        five_hour,
        weekly,
        models: None,
        plan: limits
            .get("plan_type")
            .and_then(serde_json::Value::as_str)
            .and_then(pretty_plan),
        // Overwritten centrally by `read_quota` via `prime_available_for`.
        prime_available: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    })
}

fn unix_to_rfc3339(seconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(seconds, 0).map(|dt| dt.to_rfc3339())
}

/// Normalises a raw plan string from the usage API into a short display label.
/// e.g. "plus" → "Plus", "chatgpt_pro" → "Pro", "claude_max_20x" → "Max".
fn pretty_plan(raw: &str) -> Option<String> {
    let cleaned = raw.trim().to_lowercase();
    if cleaned.is_empty() || cleaned == "free" || cleaned == "unknown" {
        return None;
    }
    // Pick the most recognisable tier keyword if present; otherwise title-case the last token.
    for tier in [
        "max",
        "pro",
        "plus",
        "team",
        "enterprise",
        "edu",
        "business",
    ] {
        if cleaned.contains(tier) {
            let mut chars = tier.chars();
            let first = chars.next().unwrap().to_uppercase().to_string();
            return Some(format!("{first}{}", chars.as_str()));
        }
    }
    let token = cleaned
        .split(['_', '-', ' '])
        .next_back()
        .unwrap_or(&cleaned);
    let mut chars = token.chars();
    let first = chars.next()?.to_uppercase().to_string();
    Some(format!("{first}{}", chars.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_background_invocation_disables_context_and_tools() {
        let args = CLAUDE_BACKGROUND_ARGS.join(" ");
        for required in [
            "--safe-mode",
            "--setting-sources",
            "--strict-mcp-config",
            "--tools",
            "--disable-slash-commands",
            "--no-chrome",
            "--permission-mode dontAsk",
            "--no-session-persistence",
        ] {
            assert!(args.contains(required), "missing hardened flag: {required}");
        }
        assert!(CLAUDE_BACKGROUND_ARGS
            .windows(2)
            .any(|pair| pair == ["--tools", ""]));
        assert!(CLAUDE_BACKGROUND_ARGS
            .windows(2)
            .any(|pair| pair == ["--setting-sources", ""]));
    }

    #[test]
    fn claude_token_validity_uses_offline_expiry_with_skew() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let blob = |expires_at: i64| {
            serde_json::json!({ "claudeAiOauth": { "accessToken": "x", "expiresAt": expires_at } })
        };
        // Comfortably in the future → valid, no refresh needed.
        assert!(claude_token_still_valid(&blob(now_ms + 60 * 60 * 1000)));
        // Already expired → needs renewal.
        assert!(!claude_token_still_valid(&blob(now_ms - 1000)));
        // Inside the skew window (expires in 5 min < 10 min skew) → treated as needing renewal so we
        // never send with a token that dies mid-prime.
        assert!(!claude_token_still_valid(&blob(now_ms + 5 * 60 * 1000)));
        // Just past the skew (expires in 11 min) → still valid.
        assert!(claude_token_still_valid(&blob(now_ms + 11 * 60 * 1000)));
        // Missing expiresAt → fail safe (needs renewal).
        assert!(!claude_token_still_valid(&serde_json::json!({ "claudeAiOauth": { "accessToken": "x" } })));
    }

    #[test]
    fn blob_expires_at_picks_newer_token() {
        let blob = |ms: i64| format!(r#"{{"claudeAiOauth":{{"accessToken":"x","expiresAt":{ms}}}}}"#);
        assert_eq!(blob_expires_at(&blob(1782426533086)), 1782426533086);
        // Missing expiresAt → 0 (treated as oldest, so a real token always wins).
        assert_eq!(blob_expires_at(r#"{"claudeAiOauth":{"accessToken":"x"}}"#), 0);
        // Unparseable → 0.
        assert_eq!(blob_expires_at("not json"), 0);
        // The seed's decision: keychain copied to file only when STRICTLY newer.
        let file = blob(2000);
        let keychain_older = blob(1000);
        let keychain_newer = blob(3000);
        assert!(blob_expires_at(&file) >= blob_expires_at(&keychain_older)); // keep file
        assert!(blob_expires_at(&file) < blob_expires_at(&keychain_newer)); // copy keychain
    }


    #[test]
    fn parses_codex_rate_limits_line() {
        let line = r#"{"timestamp":"2026-05-30T18:36:41.738Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400},"rate_limits":{"limit_id":"codex","primary":{"used_percent":60.0,"window_minutes":300,"resets_at":1780182965},"secondary":{"used_percent":21.0,"window_minutes":10080,"resets_at":1780201893},"plan_type":"plus"}}}"#;
        let limits = last_rate_limits_in(line).expect("rate_limits present");
        let quota = quota_from_codex_rate_limits(&limits).expect("quota parsed");
        assert_eq!(quota.five_hour.percent_used, Some(60.0));
        assert_eq!(quota.weekly.percent_used, Some(21.0));
        assert!(quota.five_hour.reset_at.is_some());
        assert!(quota.weekly.reset_at.is_some());
        assert_eq!(quota.plan.as_deref(), Some("Plus"));
        assert!(quota.error.is_none());
    }

    #[test]
    fn skips_null_rate_limits() {
        let line =
            r#"{"payload":{"rate_limits":{"limit_id":"codex","primary":null,"secondary":null}}}"#;
        assert!(last_rate_limits_in(line).is_none());
    }

    fn window_with_reset(secs_from_now: i64) -> QuotaWindow {
        let reset = chrono::Utc::now() + chrono::Duration::seconds(secs_from_now);
        QuotaWindow {
            label: "5-hour limit".to_string(),
            percent_used: Some(1.0),
            reset_at: Some(reset.to_rfc3339()),
            is_active: None,
        }
    }

    #[test]
    fn codex_reset_near_full_window_is_ambiguous() {
        // reset ≈ now + 5h = could be rolling OR freshly anchored → single snapshot can't tell.
        let near = window_with_reset(CODEX_FIVE_HOUR_SECONDS - 5);
        assert_eq!(
            classify_five_hour(&ToolId::Codex, &near),
            WindowState::Ambiguous
        );
    }

    #[test]
    fn codex_reset_far_from_full_window_is_anchored() {
        // reset well under now + 5h (a real request anchored it earlier) → real window → Anchored.
        let anchored = window_with_reset(CODEX_FIVE_HOUR_SECONDS - 1_000);
        assert_eq!(
            classify_five_hour(&ToolId::Codex, &anchored),
            WindowState::Anchored
        );
    }

    #[test]
    fn codex_ended_window_is_primeable() {
        let ended = window_with_reset(-60);
        assert_eq!(
            classify_five_hour(&ToolId::Codex, &ended),
            WindowState::Primeable
        );
    }

    #[test]
    fn claude_future_is_anchored_past_is_primeable() {
        assert_eq!(
            classify_five_hour(&ToolId::Claude, &window_with_reset(3_600)),
            WindowState::Anchored
        );
        assert_eq!(
            classify_five_hour(&ToolId::Claude, &window_with_reset(-60)),
            WindowState::Primeable
        );
    }

    fn quota_with(five_hour: QuotaWindow, error: Option<&str>) -> QuotaInfo {
        QuotaInfo {
            five_hour,
            weekly: QuotaWindow {
                label: "Weekly limit".to_string(),
                ..Default::default()
            },
            models: None,
            plan: None,
            prime_available: None,
            updated_at: None,
            error: error.map(str::to_string),
        }
    }

    fn empty_window() -> QuotaWindow {
        QuotaWindow {
            label: "5-hour limit".to_string(),
            percent_used: Some(0.0),
            ..Default::default()
        }
    }

    #[test]
    fn prime_available_claude_null_window_is_ended_not_unknown() {
        // resetAt None + no error = fully ended → offer prime (the xbirds bug).
        let quota = quota_with(empty_window(), None);
        assert_eq!(prime_available_for(&ToolId::Claude, &quota), Some(true));
    }

    #[test]
    fn prime_available_none_on_read_error() {
        let quota = quota_with(empty_window(), Some("Couldn't read quota"));
        assert_eq!(prime_available_for(&ToolId::Claude, &quota), None);
        assert_eq!(prime_available_for(&ToolId::Codex, &quota), None);
    }

    #[test]
    fn prime_available_codex_rolling_is_true() {
        let quota = quota_with(window_with_reset(CODEX_FIVE_HOUR_SECONDS - 5), None);
        assert_eq!(prime_available_for(&ToolId::Codex, &quota), Some(true));
    }

    #[test]
    fn prime_available_codex_anchored_is_false() {
        let quota = quota_with(window_with_reset(CODEX_FIVE_HOUR_SECONDS - 1_000), None);
        assert_eq!(prime_available_for(&ToolId::Codex, &quota), Some(false));
    }

    #[test]
    fn prime_available_antigravity_is_none() {
        let quota = quota_with(window_with_reset(3_600), None);
        assert_eq!(prime_available_for(&ToolId::Antigravity, &quota), None);
    }

    #[test]
    fn prime_available_unparseable_timestamp_is_none_not_ended() {
        // Regression guard (Codex review #2): a malformed reset_at must be unknown, not "ended".
        let bad = QuotaWindow {
            label: "5-hour limit".to_string(),
            percent_used: Some(50.0),
            reset_at: Some("not-a-timestamp".to_string()),
            is_active: None,
        };
        let quota = quota_with(bad, None);
        assert_eq!(prime_available_for(&ToolId::Claude, &quota), None);
        let bad2 = QuotaWindow {
            label: "5-hour limit".to_string(),
            percent_used: Some(50.0),
            reset_at: Some("not-a-timestamp".to_string()),
            is_active: None,
        };
        let quota2 = quota_with(bad2, None);
        assert_eq!(prime_available_for(&ToolId::Codex, &quota2), None);
    }

    #[test]
    fn claude_null_reset_is_primeable_even_without_zero_used() {
        // A successful Claude usage response with no reset means no anchored session. Utilization
        // is historical usage metadata and may be non-zero or omitted.
        let missing = QuotaWindow {
            label: "5-hour limit".to_string(),
            ..Default::default()
        };
        let quota = quota_with(missing, None);
        assert_eq!(prime_available_for(&ToolId::Claude, &quota), Some(true));

        let non_zero = QuotaWindow {
            label: "5-hour limit".to_string(),
            percent_used: Some(12.0),
            ..Default::default()
        };
        let quota = quota_with(non_zero, None);
        assert_eq!(prime_available_for(&ToolId::Claude, &quota), Some(true));
    }

    #[test]
    fn codex_null_reset_without_zero_used_is_unknown() {
        let missing2 = QuotaWindow {
            label: "5-hour limit".to_string(),
            ..Default::default()
        };
        let quota2 = quota_with(missing2, None);
        assert_eq!(prime_available_for(&ToolId::Codex, &quota2), None);
    }

    #[test]
    fn reset_near_full_window_within_and_outside_tolerance() {
        let now = 1_000_000;
        assert!(reset_is_near_full_window(
            now + CODEX_FIVE_HOUR_SECONDS,
            now
        ));
        // 80s short of now + 5h (freshly anchored 80s ago) = still within 90s tolerance.
        assert!(reset_is_near_full_window(
            now + CODEX_FIVE_HOUR_SECONDS - 80,
            now
        ));
        // 1000s short = clearly an anchored window = outside tolerance.
        assert!(!reset_is_near_full_window(
            now + CODEX_FIVE_HOUR_SECONDS - 1_000,
            now
        ));
    }

    #[test]
    fn parses_antigravity_user_status() {
        let body = r#"{"userStatus":{"name":"Designer","planStatus":{"planInfo":{"planName":"Pro"}},"cascadeModelConfigData":{"clientModelConfigs":[{"label":"Gemini 3.1 Pro","quotaInfo":{"remainingFraction":1,"resetTime":"2026-05-31T12:14:05Z"}},{"label":"Claude Opus","quotaInfo":{"remainingFraction":0.4,"resetTime":"2026-05-31T13:00:00Z"}}]}}}"#;
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        let quota = quota_from_antigravity_status(&value).unwrap();
        // the model with the least remaining is 0.4 → 60% used
        assert_eq!(quota.five_hour.percent_used, Some(60.0));
        assert_eq!(
            quota.five_hour.reset_at.as_deref(),
            Some("2026-05-31T13:00:00Z")
        );
        assert!(quota.weekly.percent_used.is_none());
        assert_eq!(quota.plan.as_deref(), Some("Pro"));
    }

    #[test]
    fn antigravity_missing_remaining_fraction_is_exhausted() {
        // proto3 JSON omits a field = its default value: an exhausted model has only
        // resetTime, no remainingFraction. This model must NOT be skipped.
        let body = r#"{"userStatus":{"cascadeModelConfigData":{"clientModelConfigs":[
            {"label":"Gemini 3.5 Flash","quotaInfo":{"remainingFraction":1,"resetTime":"2026-05-31T12:42:25Z"}},
            {"label":"Claude Opus 4.6 (Thinking)","quotaInfo":{"resetTime":"2026-05-31T12:48:49Z"}}
        ]}}}"#;
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        let quota = quota_from_antigravity_status(&value).unwrap();
        let models = quota.models.expect("models present");
        assert_eq!(models.len(), 2);
        let claude = models
            .iter()
            .find(|m| m.label.contains("Claude"))
            .expect("claude model kept");
        assert_eq!(claude.percent_used, Some(100.0));
        // worst = 100% (Claude exhausted), not 0% (Gemini full).
        assert_eq!(quota.five_hour.percent_used, Some(100.0));
    }

    #[test]
    fn parses_codex_wham_usage() {
        let body = r#"{"plan_type":"plus","rate_limit":{"primary_window":{"used_percent":1,"limit_window_seconds":18000,"reset_at":1780229541},"secondary_window":{"used_percent":0,"limit_window_seconds":604800,"reset_at":1780816341}}}"#;
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        let quota = quota_from_codex_endpoint(&value).unwrap();
        assert_eq!(quota.five_hour.percent_used, Some(1.0));
        assert_eq!(quota.weekly.percent_used, Some(0.0));
        assert!(quota.five_hour.reset_at.is_some());
        assert!(quota.weekly.reset_at.is_some());
        assert_eq!(quota.plan.as_deref(), Some("Plus"));
    }

    #[test]
    fn parses_claude_oauth_usage() {
        let body = r#"{"five_hour":{"utilization":4.0,"resets_at":"2026-05-31T11:00:00.033919+00:00"},"seven_day":{"utilization":14.0,"resets_at":"2026-06-05T03:00:00.033953+00:00"},"seven_day_sonnet":{"utilization":0.0,"resets_at":null},"extra_usage":{"is_enabled":false}}"#;
        let value: serde_json::Value = serde_json::from_str(body).unwrap();
        let quota = quota_from_claude_usage(&value).unwrap();
        assert_eq!(quota.five_hour.percent_used, Some(4.0));
        assert_eq!(quota.weekly.percent_used, Some(14.0));
        assert_eq!(
            quota.weekly.reset_at.as_deref(),
            Some("2026-06-05T03:00:00.033953+00:00")
        );
        assert!(quota.error.is_none());
    }

    #[test]
    fn claude_keychain_suffix_is_sha256_prefix() {
        // Values verified for real using `security` + sha256 on the machine:
        //   profile dir namtran → "Claude Code-credentials-e3c60653" (quota read OK)
        //   temp profile login-test → "Claude Code-credentials-8244da8e"
        let profile = claude_keychain_suffix(Path::new(
            "/Users/hoangphan/Library/Application Support/dev.hoangphan.AI-Account-Switcher/accounts/claude/53d79dfb-fbe4-41fb-9827-e8afd2e128bb",
        ));
        assert_eq!(profile, "e3c60653");
        // Different path → different suffix (each profile has its own separate credential).
        let other = claude_keychain_suffix(Path::new("/Users/hoangphan/.ai-switcher-logintest"));
        assert_eq!(other, "8244da8e");
        assert_ne!(profile, other);
    }

    #[test]
    fn keeps_last_populated_entry() {
        let text = concat!(
            r#"{"payload":{"rate_limits":{"primary":{"used_percent":10.0,"window_minutes":300,"resets_at":1780182965}}}}"#,
            "\n",
            r#"{"payload":{"rate_limits":{"primary":{"used_percent":42.0,"window_minutes":300,"resets_at":1780182965}}}}"#,
        );
        let limits = last_rate_limits_in(text).expect("present");
        let quota = quota_from_codex_rate_limits(&limits).unwrap();
        assert_eq!(quota.five_hour.percent_used, Some(42.0));
    }

    #[test]
    fn reads_codex_account_id_separately_from_access_token() {
        let dir = std::env::temp_dir().join(format!("aisw-codex-auth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"tokens":{"access_token":"secret-token","account_id":"acct_123"}}"#,
        )
        .unwrap();

        assert_eq!(codex_account_id(&dir).as_deref(), Some("acct_123"));
        let _ = std::fs::remove_dir_all(dir);
    }
}

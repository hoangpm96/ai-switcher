use crate::models::{QuotaInfo, QuotaWindow, ToolId};
use crate::tools::home_dir;
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// `config_dir` is the `CLAUDE_CONFIG_DIR` of the account being read (profile dir,
/// or `~/.claude` for the default account). Claude stores the keychain token by the
/// hash of this path, so the correct dir must be passed to read each account's quota.
pub fn read_quota(tool_id: &ToolId, config_dir: &Path) -> QuotaInfo {
    let result = match tool_id {
        ToolId::Codex => read_codex_quota(),
        ToolId::Claude => read_claude_quota(config_dir),
        ToolId::Antigravity => read_antigravity_quota(),
    };

    result.unwrap_or_else(|_| match tool_id {
        // Antigravity only exposes quota while the IDE is open (language server runs locally).
        ToolId::Antigravity => QuotaInfo::with_message("Open Antigravity IDE to read quota"),
        other => QuotaInfo::unavailable(other.display_name()),
    })
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

fn read_claude_quota(config_dir: &Path) -> Result<QuotaInfo> {
    let cache_key = config_dir.to_string_lossy().to_string();
    if let Ok(guard) = CLAUDE_CACHE.lock() {
        if let Some((fetched_at, quota)) = guard.get(&cache_key) {
            if fetched_at.elapsed() < CLAUDE_CACHE_TTL {
                return Ok(quota.clone());
            }
        }
    }

    let token =
        claude_oauth_token(config_dir).context("couldn't get Claude's OAuth token")?;
    let version = claude_version().unwrap_or_else(|| "0.0.0".to_string());
    let user_agent = format!("claude-code/{version}");
    let body = curl_get(
        "https://api.anthropic.com/api/oauth/usage",
        &[
            ("Authorization", format!("Bearer {token}").as_str()),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("User-Agent", user_agent.as_str()),
            ("Accept", "application/json"),
        ],
    )?;

    let value: serde_json::Value =
        serde_json::from_str(&body).context("Claude usage response is not JSON")?;
    let quota = quota_from_claude_usage(&value)?;

    if let Ok(mut guard) = CLAUDE_CACHE.lock() {
        guard.insert(cache_key, (Instant::now(), quota.clone()));
    }
    Ok(quota)
}

fn quota_from_claude_usage(value: &serde_json::Value) -> Result<QuotaInfo> {
    let five_hour = claude_window("5-hour limit", value.get("five_hour"));
    let weekly = claude_window("Weekly limit", value.get("seven_day"));

    if five_hour.percent_used.is_none() && weekly.percent_used.is_none() {
        anyhow::bail!("Claude usage has no utilization");
    }

    Ok(QuotaInfo {
        five_hour,
        weekly,
        models: None,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    })
}

fn claude_window(label: &str, value: Option<&serde_json::Value>) -> QuotaWindow {
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
    }
}

fn claude_oauth_token(config_dir: &Path) -> Option<String> {
    let raw = claude_credentials_blob(config_dir)?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("accessToken"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
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

/// Gets Claude's JSON credential for a specific config dir.
///
/// Order: (1) per-dir keychain by hash (Claude 2.x) → (2) the
/// `.credentials.json` file inside the config dir itself (older versions stored a per-dir file) → (3)
/// ONLY when it is the default dir `~/.claude`: try the old global keychain name (compatible
/// with very old versions). Do NOT fall back to the global one for a profile dir, to avoid reading
/// the wrong account's token.
fn claude_credentials_blob(config_dir: &Path) -> Option<String> {
    let suffix = claude_keychain_suffix(config_dir);
    if let Some(blob) = read_keychain_blob(&format!("Claude Code-credentials-{suffix}")) {
        return Some(blob);
    }

    if let Some(blob) = read_file_blob(&config_dir.join(".credentials.json")) {
        return Some(blob);
    }

    if config_dir == home_dir().join(".claude") {
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

fn curl_get(url: &str, headers: &[(&str, &str)]) -> Result<String> {
    let mut command = Command::new("curl");
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail") // exit != 0 when HTTP >= 400 (e.g. 401/429)
        .arg("--max-time")
        .arg("20");
    for (key, value) in headers {
        command.arg("-H").arg(format!("{key}: {value}"));
    }
    command.arg(url);

    let output = command.output().context("couldn't run curl")?;
    if !output.status.success() {
        anyhow::bail!("HTTP request failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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
        },
        weekly: QuotaWindow {
            label: "Weekly limit".to_string(),
            percent_used: None,
            reset_at: None,
        },
        models: Some(models),
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

fn read_codex_quota() -> Result<QuotaInfo> {
    // Prefer the local rollout file (offline, no request cost). Newer Codex versions
    // sometimes write rate_limits = null into the rollout → fall back to the wham/usage endpoint.
    match read_codex_rollout_quota() {
        Ok(quota) => Ok(quota),
        Err(_) => read_codex_usage_endpoint(),
    }
}

fn read_codex_rollout_quota() -> Result<QuotaInfo> {
    let sessions = home_dir().join(".codex/sessions");
    let limits = latest_codex_rate_limits(&sessions)
        .context("rate_limits not found in the Codex session")?;
    quota_from_codex_rate_limits(&limits)
}

/// Fallback: calls `GET https://chatgpt.com/backend-api/wham/usage` with the JWT in
/// `~/.codex/auth.json` (`tokens.access_token`). Returns rate_limit.primary_window
/// (5h, limit_window_seconds 18000) + secondary_window (weekly, 604800), each with
/// `used_percent` + `reset_at` (unix seconds).
fn read_codex_usage_endpoint() -> Result<QuotaInfo> {
    let token = codex_access_token().context("couldn't get Codex's access_token")?;
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

fn codex_access_token() -> Option<String> {
    let raw = std::fs::read_to_string(home_dir().join(".codex/auth.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

fn quota_from_codex_endpoint(value: &serde_json::Value) -> Result<QuotaInfo> {
    let rate_limit = value
        .get("rate_limit")
        .context("Codex response is missing rate_limit")?;

    let mut five_hour = QuotaWindow {
        label: "5-hour limit".to_string(),
        percent_used: None,
        reset_at: None,
    };
    let mut weekly = QuotaWindow {
        label: "Weekly limit".to_string(),
        percent_used: None,
        reset_at: None,
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
        percent_used: None,
        reset_at: None,
    };
    let mut weekly = QuotaWindow {
        label: "Weekly limit".to_string(),
        percent_used: None,
        reset_at: None,
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
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
        error: None,
    })
}

fn unix_to_rfc3339(seconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(seconds, 0).map(|dt| dt.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_codex_rate_limits_line() {
        let line = r#"{"timestamp":"2026-05-30T18:36:41.738Z","type":"event_msg","payload":{"type":"token_count","info":{"model_context_window":258400},"rate_limits":{"limit_id":"codex","primary":{"used_percent":60.0,"window_minutes":300,"resets_at":1780182965},"secondary":{"used_percent":21.0,"window_minutes":10080,"resets_at":1780201893},"plan_type":"plus"}}}"#;
        let limits = last_rate_limits_in(line).expect("rate_limits present");
        let quota = quota_from_codex_rate_limits(&limits).expect("quota parsed");
        assert_eq!(quota.five_hour.percent_used, Some(60.0));
        assert_eq!(quota.weekly.percent_used, Some(21.0));
        assert!(quota.five_hour.reset_at.is_some());
        assert!(quota.weekly.reset_at.is_some());
        assert!(quota.error.is_none());
    }

    #[test]
    fn skips_null_rate_limits() {
        let line =
            r#"{"payload":{"rate_limits":{"limit_id":"codex","primary":null,"secondary":null}}}"#;
        assert!(last_rate_limits_in(line).is_none());
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
}

// ---------------------------------------------------------------------------
// Token usage tracking — aggregates token counts + USD cost from the CLIs' local
// JSONL logs, for the "Usage" tab. Aggregated per tool (Claude / Codex), NOT per
// account, across every config dir on the machine.
//
//   Claude: <config_dir>/projects/**/*.jsonl — each assistant message line carries
//           `message.usage` (input/output/cache tokens). These logs UNDERCOUNT badly
//           (placeholder values during streaming), so Claude numbers are an estimate.
//   Codex:  <config_dir>/sessions/**/rollout-*.jsonl — each `token_count` event carries
//           a CUMULATIVE `total_token_usage`; per-turn usage = delta between events. The
//           active model comes from the latest `turn_context`. These numbers are accurate.
//
// Reading is incremental: a per-file byte cursor means each line is parsed once, so a
// refresh only reads what's new. The aggregates live in `usage.json` next to state.json.
// ---------------------------------------------------------------------------

use crate::models::{
    DayUsage, ModelUsage, SessionUsage, TokenBreakdown, ToolId, ToolUsage, UsageReport,
};
use crate::pricing::{load_price_table, PriceTable};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Cap on sessions returned per tool in the report (newest first).
const MAX_SESSIONS: usize = 30;

/// Bump when the cache format or scan logic changes in a way that invalidates old aggregates
/// (e.g. the symlink-dedup fix) so a stale cache is discarded instead of double-counting.
const CACHE_VERSION: u32 = 2;

// ---------------------------------------------------------------------------
// On-disk incremental cache
// ---------------------------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageCache {
    /// Format version — a mismatch discards the cache (see CACHE_VERSION).
    #[serde(default)]
    version: u32,
    /// Canonical JSONL file path → read cursor (so each line is only counted once).
    files: BTreeMap<String, FileCursor>,
    /// "tool|YYYY-MM-DD|model" → token totals (drives daily + per-model views).
    buckets: BTreeMap<String, TokenBreakdown>,
    /// JSONL file path → session summary (each file is one session).
    sessions: BTreeMap<String, SessionRecord>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileCursor {
    /// Bytes already consumed (always at a line boundary).
    offset: u64,
    /// Codex only: last cumulative totals seen, carried across refreshes so deltas stay correct.
    #[serde(default)]
    codex_input: u64,
    #[serde(default)]
    codex_cached: u64,
    #[serde(default)]
    codex_output: u64,
    /// Codex only: the model in effect at the cursor (latest turn_context).
    #[serde(default)]
    codex_model: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRecord {
    tool: String,
    id: String,
    date: String,
    model: String,
    tokens: TokenBreakdown,
}

fn load_cache(path: &Path) -> UsageCache {
    let cache: UsageCache = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    // Discard a cache written by an older version (its aggregates may be wrong).
    if cache.version == CACHE_VERSION {
        cache
    } else {
        UsageCache::default()
    }
}

fn save_cache(path: &Path, cache: &UsageCache) {
    if let Ok(bytes) = serde_json::to_vec(cache) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Scan the given config dirs incrementally, persist the cache, then build the report.
pub fn build_report(
    cache_path: &Path,
    price_cache_path: &Path,
    claude_dirs: &[PathBuf],
    codex_dirs: &[PathBuf],
) -> UsageReport {
    let mut cache = load_cache(cache_path);

    // The app symlinks the shared session store across profile dirs + the machine default, so the
    // same physical JSONL is reachable from several config dirs. Resolve symlinks and scan each
    // real file once, or its tokens get counted 2-3x.
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for dir in claude_dirs {
        for file in collect_jsonl(&dir.join("projects"), "") {
            let real = std::fs::canonicalize(&file).unwrap_or(file);
            if seen.insert(real.clone()) {
                scan_claude_file(&real, &mut cache);
            }
        }
    }
    for dir in codex_dirs {
        for file in collect_jsonl(&dir.join("sessions"), "rollout-") {
            let real = std::fs::canonicalize(&file).unwrap_or(file);
            if seen.insert(real.clone()) {
                scan_codex_file(&real, &mut cache);
            }
        }
    }

    cache.version = CACHE_VERSION;
    save_cache(cache_path, &cache);

    let prices = load_price_table(price_cache_path);
    build_report_from_cache(&cache, &prices)
}

// ---------------------------------------------------------------------------
// Incremental file reading
// ---------------------------------------------------------------------------

/// Reads the bytes appended since `offset`. Returns the complete lines plus the new offset
/// (advanced only past the last newline, so a half-written final line waits for next time).
fn read_new_lines(path: &Path, offset: u64) -> Option<(Vec<String>, u64)> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    // File shrank (rotated/replaced) → start over.
    let start = if offset > len { 0 } else { offset };
    if start == len {
        return Some((Vec::new(), start));
    }
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    let consumed = match buf.rfind('\n') {
        Some(idx) => idx + 1,
        None => 0,
    };
    let lines = buf[..consumed].lines().map(ToString::to_string).collect();
    Some((lines, start + consumed as u64))
}

/// Recursively collects `*.jsonl` files under `dir` whose name starts with `name_prefix`.
fn collect_jsonl(dir: &Path, name_prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_jsonl_into(dir, name_prefix, &mut out);
    out
}

fn collect_jsonl_into(dir: &Path, name_prefix: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_into(&path, name_prefix, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            let matches = name_prefix.is_empty()
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(name_prefix));
            if matches {
                out.push(path);
            }
        }
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session")
        .to_string()
}

// ---------------------------------------------------------------------------
// Claude — one assistant message per relevant line; usage tokens are independent fields.
// ---------------------------------------------------------------------------

fn scan_claude_file(path: &Path, cache: &mut UsageCache) {
    let key = path.to_string_lossy().to_string();
    let offset = cache.files.get(&key).map(|c| c.offset).unwrap_or(0);
    let Some((lines, new_offset)) = read_new_lines(path, offset) else {
        return;
    };
    if lines.is_empty() {
        cache.files.entry(key).or_default().offset = new_offset;
        return;
    }

    // Dedup the same assistant message appearing on multiple streaming lines: keep the
    // entry with the most tokens per message id (within this batch).
    let mut best: BTreeMap<String, ClaudeEntry> = BTreeMap::new();
    for line in &lines {
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(entry) = claude_entry(&value) else {
            continue;
        };
        match best.get(&entry.id) {
            Some(existing) if existing.tokens.total() >= entry.tokens.total() => {}
            _ => {
                best.insert(entry.id.clone(), entry);
            }
        }
    }

    let session_id = file_stem(path);
    for entry in best.into_values() {
        add_bucket(cache, "claude", &entry.date, &entry.model, &entry.tokens);
        add_session(cache, &key, "claude", &session_id, &entry.date, &entry.model, &entry.tokens);
    }

    cache.files.entry(key).or_default().offset = new_offset;
}

struct ClaudeEntry {
    id: String,
    model: String,
    date: String,
    tokens: TokenBreakdown,
}

/// Extracts a usage entry from a Claude transcript line (assistant message), if present.
fn claude_entry(value: &serde_json::Value) -> Option<ClaudeEntry> {
    let message = value.get("message")?;
    if message.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return None;
    }
    let usage = message.get("usage")?;
    let tokens = TokenBreakdown {
        input: u64_at(usage, "input_tokens"),
        output: u64_at(usage, "output_tokens"),
        cache_read: u64_at(usage, "cache_read_input_tokens"),
        cache_creation: u64_at(usage, "cache_creation_input_tokens"),
    };
    if tokens.total() == 0 {
        return None;
    }
    let model = message
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();
    let date = value
        .get("timestamp")
        .and_then(|t| t.as_str())
        .and_then(local_date)
        .unwrap_or_else(unknown_date);
    // Prefer the message id; fall back to the line uuid so undated lines still dedup sanely.
    let id = message
        .get("id")
        .and_then(|i| i.as_str())
        .or_else(|| value.get("uuid").and_then(|u| u.as_str()))
        .unwrap_or("")
        .to_string();
    Some(ClaudeEntry { id, model, date, tokens })
}

// ---------------------------------------------------------------------------
// Codex — cumulative token_count events; per-turn usage is the delta. Model from turn_context.
// ---------------------------------------------------------------------------

fn scan_codex_file(path: &Path, cache: &mut UsageCache) {
    let key = path.to_string_lossy().to_string();
    let cursor = cache.files.get(&key);
    let offset = cursor.map(|c| c.offset).unwrap_or(0);
    let mut last_input = cursor.map(|c| c.codex_input).unwrap_or(0);
    let mut last_cached = cursor.map(|c| c.codex_cached).unwrap_or(0);
    let mut last_output = cursor.map(|c| c.codex_output).unwrap_or(0);
    let mut model = cursor
        .map(|c| c.codex_model.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let Some((lines, new_offset)) = read_new_lines(path, offset) else {
        return;
    };

    let session_id = file_stem(path);
    for line in &lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(found) = codex_model(&value) {
            model = found;
            continue;
        }
        let Some(total) = codex_total_usage(&value) else {
            continue;
        };
        // Delta from the last cumulative snapshot (reset to the raw total if it went backwards).
        let growing = total.input >= last_input && total.output >= last_output;
        let (d_input, d_cached, d_output) = if growing {
            (
                total.input - last_input,
                total.cached.saturating_sub(last_cached),
                total.output - last_output,
            )
        } else {
            (total.input, total.cached, total.output)
        };
        last_input = total.input;
        last_cached = total.cached;
        last_output = total.output;

        let tokens = TokenBreakdown {
            // Codex `input_tokens` includes cached input → split it out so cost is correct.
            input: d_input.saturating_sub(d_cached),
            output: d_output,
            cache_read: d_cached,
            cache_creation: 0,
        };
        if tokens.total() == 0 {
            continue;
        }
        let date = total.date.clone().unwrap_or_else(unknown_date);
        add_bucket(cache, "codex", &date, &model, &tokens);
        add_session(cache, &key, "codex", &session_id, &date, &model, &tokens);
    }

    let entry = cache.files.entry(key).or_default();
    entry.offset = new_offset;
    entry.codex_input = last_input;
    entry.codex_cached = last_cached;
    entry.codex_output = last_output;
    entry.codex_model = model;
}

struct CodexTotal {
    input: u64,
    cached: u64,
    output: u64,
    date: Option<String>,
}

/// Extracts the cumulative `total_token_usage` from a Codex `token_count` event line.
fn codex_total_usage(value: &serde_json::Value) -> Option<CodexTotal> {
    let payload = value.get("payload")?;
    if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
        return None;
    }
    let total = payload.get("info")?.get("total_token_usage")?;
    Some(CodexTotal {
        input: u64_at(total, "input_tokens"),
        cached: u64_at(total, "cached_input_tokens"),
        output: u64_at(total, "output_tokens"),
        date: value
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(local_date),
    })
}

/// Extracts the active model from a Codex `turn_context` line.
fn codex_model(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|t| t.as_str()) != Some("turn_context") {
        return None;
    }
    value
        .get("payload")?
        .get("model")?
        .as_str()
        .map(ToString::to_string)
}

// ---------------------------------------------------------------------------
// Aggregation helpers
// ---------------------------------------------------------------------------

fn add_bucket(cache: &mut UsageCache, tool: &str, date: &str, model: &str, tokens: &TokenBreakdown) {
    let key = format!("{tool}|{date}|{model}");
    cache.buckets.entry(key).or_default().add(tokens);
}

fn add_session(
    cache: &mut UsageCache,
    path_key: &str,
    tool: &str,
    id: &str,
    date: &str,
    model: &str,
    tokens: &TokenBreakdown,
) {
    let record = cache.sessions.entry(path_key.to_string()).or_insert_with(|| SessionRecord {
        tool: tool.to_string(),
        id: id.to_string(),
        date: date.to_string(),
        model: model.to_string(),
        tokens: TokenBreakdown::default(),
    });
    record.tokens.add(tokens);
    // Track the latest activity date + the model in use at that point.
    if date >= record.date.as_str() {
        record.date = date.to_string();
        record.model = model.to_string();
    }
}

fn u64_at(value: &serde_json::Value, key: &str) -> u64 {
    value.get(key).and_then(|v| v.as_u64()).unwrap_or(0)
}

fn local_date(ts: &str) -> Option<String> {
    let datetime = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(
        datetime
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string(),
    )
}

fn unknown_date() -> String {
    "unknown".to_string()
}

fn today_local() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Sums optional costs: Some when at least one item is priced, None when nothing is priced.
fn sum_cost(items: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = 0.0;
    let mut any = false;
    for item in items {
        if let Some(value) = item {
            total += value;
            any = true;
        }
    }
    any.then_some(total)
}

// ---------------------------------------------------------------------------
// Report building
// ---------------------------------------------------------------------------

fn build_report_from_cache(cache: &UsageCache, prices: &PriceTable) -> UsageReport {
    let today = today_local();
    let tools = [ToolId::Claude, ToolId::Codex]
        .into_iter()
        .map(|tool_id| tool_usage(cache, prices, &tool_id, &today))
        .collect();

    UsageReport {
        tools,
        generated_at: chrono::Utc::now().to_rfc3339(),
        price_status: prices.status.clone(),
        price_updated_at: prices.updated_at.clone(),
    }
}

fn tool_usage(
    cache: &UsageCache,
    prices: &PriceTable,
    tool_id: &ToolId,
    today: &str,
) -> ToolUsage {
    let tool = tool_id.as_str();
    let prefix = format!("{tool}|");

    let mut daily: BTreeMap<String, TokenBreakdown> = BTreeMap::new();
    let mut day_cost: BTreeMap<String, Option<f64>> = BTreeMap::new();
    let mut by_model: BTreeMap<String, TokenBreakdown> = BTreeMap::new();
    let mut total = TokenBreakdown::default();
    let mut today_tokens = TokenBreakdown::default();

    for (key, tokens) in cache.buckets.iter().filter(|(k, _)| k.starts_with(&prefix)) {
        // key = "tool|date|model" — model may itself contain '|'? Model names never do.
        let mut parts = key.splitn(3, '|');
        let _ = parts.next();
        let date = parts.next().unwrap_or("unknown").to_string();
        let model = parts.next().unwrap_or("unknown").to_string();

        total.add(tokens);
        daily.entry(date.clone()).or_default().add(tokens);
        by_model.entry(model.clone()).or_default().add(tokens);

        let cost = prices.cost(&model, tokens);
        let day = day_cost.entry(date.clone()).or_insert(None);
        *day = sum_cost([*day, cost].into_iter());

        if date == today {
            today_tokens.add(tokens);
        }
    }

    let daily: Vec<DayUsage> = daily
        .into_iter()
        .map(|(date, tokens)| {
            let cost_usd = day_cost.get(&date).copied().flatten();
            DayUsage { date, tokens, cost_usd }
        })
        .collect();

    let mut by_model: Vec<ModelUsage> = by_model
        .into_iter()
        .map(|(model, tokens)| {
            let cost_usd = prices.cost(&model, &tokens);
            ModelUsage { model, tokens, cost_usd }
        })
        .collect();
    by_model.sort_by(|a, b| b.tokens.total().cmp(&a.tokens.total()));

    let total_cost_usd = sum_cost(by_model.iter().map(|m| m.cost_usd));
    let today_cost_usd = day_cost.get(today).copied().flatten();

    let mut sessions: Vec<SessionUsage> = cache
        .sessions
        .values()
        .filter(|record| record.tool == tool)
        .map(|record| SessionUsage {
            id: record.id.clone(),
            date: record.date.clone(),
            model: record.model.clone(),
            tokens: record.tokens,
            cost_usd: prices.cost(&record.model, &record.tokens),
        })
        .collect();
    sessions.sort_by(|a, b| b.date.cmp(&a.date).then(b.tokens.total().cmp(&a.tokens.total())));
    sessions.truncate(MAX_SESSIONS);

    ToolUsage {
        tool_id: tool_id.clone(),
        display_name: tool_id.display_name().to_string(),
        estimate: matches!(tool_id, ToolId::Claude),
        total,
        total_cost_usd,
        today: today_tokens,
        today_cost_usd,
        daily,
        by_model,
        sessions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_assistant_usage() {
        let line = r#"{"message":{"model":"claude-sonnet-4-5-20250929","id":"msg_1","role":"assistant","usage":{"input_tokens":10,"cache_creation_input_tokens":43047,"cache_read_input_tokens":12914,"output_tokens":3720}},"requestId":"req_1","timestamp":"2026-05-10T04:21:44.283Z"}"#;
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let entry = claude_entry(&value).expect("usage parsed");
        assert_eq!(entry.id, "msg_1");
        assert_eq!(entry.model, "claude-sonnet-4-5-20250929");
        assert_eq!(entry.tokens.input, 10);
        assert_eq!(entry.tokens.cache_creation, 43047);
        assert_eq!(entry.tokens.cache_read, 12914);
        assert_eq!(entry.tokens.output, 3720);
    }

    #[test]
    fn skips_non_assistant_and_zero_usage() {
        let user = r#"{"message":{"role":"user","content":"hi"},"timestamp":"2026-05-10T04:21:44.283Z"}"#;
        assert!(claude_entry(&serde_json::from_str(user).unwrap()).is_none());
        let zero = r#"{"message":{"role":"assistant","id":"m","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"timestamp":"2026-05-10T04:21:44.283Z"}"#;
        assert!(claude_entry(&serde_json::from_str(zero).unwrap()).is_none());
    }

    #[test]
    fn parses_codex_cumulative_usage() {
        let line = r#"{"timestamp":"2026-06-02T13:17:24.505Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":11861,"cached_input_tokens":9088,"output_tokens":17,"reasoning_output_tokens":0,"total_tokens":11878}}}}"#;
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        let total = codex_total_usage(&value).expect("token_count parsed");
        assert_eq!(total.input, 11861);
        assert_eq!(total.cached, 9088);
        assert_eq!(total.output, 17);
    }

    #[test]
    fn reads_codex_model_from_turn_context() {
        let line = r#"{"type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.5"}}"#;
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(codex_model(&value).as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn codex_delta_splits_cached_input() {
        // Two cumulative snapshots in one file → second turn's delta is the difference.
        let mut cache = UsageCache::default();
        let tmp = std::env::temp_dir().join("aisw_codex_test_rollout-x.jsonl");
        let content = concat!(
            r#"{"type":"turn_context","payload":{"model":"gpt-5.5"}}"#, "\n",
            r#"{"timestamp":"2026-06-02T13:17:24.505Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":10}}}}"#, "\n",
            r#"{"timestamp":"2026-06-02T13:18:24.505Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":250,"cached_input_tokens":90,"output_tokens":30}}}}"#, "\n",
        );
        std::fs::write(&tmp, content).unwrap();
        scan_codex_file(&tmp, &mut cache);
        let _ = std::fs::remove_file(&tmp);

        // Totals across both turns: input(non-cached)=250-90=160 cached -> input field;
        // cache_read = 90 cached; output = 30. (first 100/40/10 + delta 150/50/20)
        let total: TokenBreakdown = cache.buckets.values().fold(TokenBreakdown::default(), |mut acc, t| {
            acc.add(t);
            acc
        });
        assert_eq!(total.cache_read, 90); // 40 + 50
        assert_eq!(total.output, 30); // 10 + 20
        assert_eq!(total.input, 160); // (100-40) + (150-50) = 60 + 100
    }


    #[test]
    fn dedups_symlinked_files_across_config_dirs() {
        // The shared-session symlinks make one physical file reachable from several config dirs.
        // build_report must count it once, not once per dir.
        use std::os::unix::fs::symlink;
        let base = std::env::temp_dir().join(format!("aisw_dedup_{}", std::process::id()));
        let default_projects = base.join("default/projects/sub");
        let profile_projects = base.join("profile/projects");
        std::fs::create_dir_all(&default_projects).unwrap();
        std::fs::create_dir_all(&profile_projects).unwrap();

        let real = default_projects.join("conv.jsonl");
        let line = r#"{"message":{"model":"claude-x","id":"m1","role":"assistant","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"timestamp":"2026-06-01T10:00:00.000Z"}"#;
        std::fs::write(&real, format!("{line}\n")).unwrap();
        // The profile's projects dir is a symlink to the default's (mirrors link_shared_sessions).
        let _ = std::fs::remove_dir(&profile_projects);
        symlink(base.join("default/projects"), &profile_projects).unwrap();

        let cache = base.join("usage.json");
        let prices = base.join("prices.json"); // missing → no cost, fine for token assert
        let claude_dirs = vec![base.join("default"), base.join("profile")];
        let report = build_report(&cache, &prices, &claude_dirs, &[]);
        let _ = std::fs::remove_dir_all(&base);

        let claude = report.tools.iter().find(|t| t.tool_id == ToolId::Claude).unwrap();
        // Counted once: 100 input + 50 output, not doubled.
        assert_eq!(claude.total.input, 100);
        assert_eq!(claude.total.output, 50);
        assert_eq!(claude.sessions.len(), 1);
    }

    #[test]
    fn incremental_cursor_skips_already_read_lines() {
        let mut cache = UsageCache::default();
        let tmp = std::env::temp_dir().join("aisw_claude_test_incr.jsonl");
        let line1 = r#"{"message":{"model":"claude-x","id":"m1","role":"assistant","usage":{"input_tokens":5,"output_tokens":7,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"timestamp":"2026-06-01T10:00:00.000Z"}"#;
        std::fs::write(&tmp, format!("{line1}\n")).unwrap();
        scan_claude_file(&tmp, &mut cache);

        // Append a second message and rescan — only the new line should be added.
        let line2 = r#"{"message":{"model":"claude-x","id":"m2","role":"assistant","usage":{"input_tokens":3,"output_tokens":4,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"timestamp":"2026-06-01T10:05:00.000Z"}"#;
        let mut file = std::fs::OpenOptions::new().append(true).open(&tmp).unwrap();
        use std::io::Write;
        writeln!(file, "{line2}").unwrap();
        scan_claude_file(&tmp, &mut cache);
        let _ = std::fs::remove_file(&tmp);

        let total: TokenBreakdown = cache.buckets.values().fold(TokenBreakdown::default(), |mut acc, t| {
            acc.add(t);
            acc
        });
        // m1 (5+7) + m2 (3+4) counted exactly once each.
        assert_eq!(total.input, 8);
        assert_eq!(total.output, 11);
    }
}

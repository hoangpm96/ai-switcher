use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ToolId {
    Claude,
    Codex,
    Antigravity,
}

impl ToolId {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolId::Claude => "claude",
            ToolId::Codex => "codex",
            ToolId::Antigravity => "antigravity",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ToolId::Claude => "Claude Code",
            ToolId::Codex => "Codex",
            ToolId::Antigravity => "Antigravity IDE",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AccountState {
    Idle,
    Active,
    Exhausted,
    NeedsLogin,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub label: String,
    pub percent_used: Option<f64>,
    pub reset_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaInfo {
    pub five_hour: QuotaWindow,
    pub weekly: QuotaWindow,
    /// Per-model quota detail (Antigravity). None for tools that only have a
    /// single overall window (Claude, Codex).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<QuotaWindow>>,
    pub updated_at: Option<String>,
    pub error: Option<String>,
}

impl QuotaInfo {
    pub fn unavailable(tool_name: &str) -> Self {
        Self {
            five_hour: QuotaWindow {
                label: "5-hour limit".to_string(),
                percent_used: None,
                reset_at: None,
            },
            weekly: QuotaWindow {
                label: "Weekly limit".to_string(),
                percent_used: None,
                reset_at: None,
            },
            models: None,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
            error: Some(format!("Couldn't read quota — check {tool_name}")),
        }
    }

    /// Empty QuotaInfo with a custom error message (e.g. "Open Antigravity IDE…").
    pub fn with_message(message: impl Into<String>) -> Self {
        Self {
            five_hour: QuotaWindow {
                label: "5-hour limit".to_string(),
                percent_used: None,
                reset_at: None,
            },
            weekly: QuotaWindow {
                label: "Weekly limit".to_string(),
                percent_used: None,
                reset_at: None,
            },
            models: None,
            updated_at: Some(chrono::Utc::now().to_rfc3339()),
            error: Some(message.into()),
        }
    }
}

/// API/proxy provider config for an account that runs a CLI tool through an external
/// gateway (API key) instead of a subscription OAuth login. The API key itself is NOT
/// stored here — it lives in a file inside the account's profile dir (`api_key`).
///
/// One account = one pinned gateway model: Codex's `/model` picker can't resolve gateway
/// ids, so the launcher forces `-m <model>` and the account runs only this model. Use a
/// separate account for a different model/effort.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiProvider {
    /// Gateway base URL, e.g. `https://your-gateway.com/v1`. Models are listed at `{base_url}/models`.
    pub base_url: String,
    /// The gateway model id the account runs (written as `model = "…"` and forced via `-m`).
    /// `alias` reads accounts saved by the earlier `defaultModel` schema; an unknown `modelMap`
    /// key from that schema is simply ignored on load.
    #[serde(alias = "defaultModel")]
    pub model: String,
    /// Add `--dangerously-bypass-approvals-and-sandbox` to the account's launcher. Default off.
    #[serde(default)]
    pub bypass: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub tool_id: ToolId,
    pub name: String,
    pub state: AccountState,
    pub fingerprint: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
    pub quota: Option<QuotaInfo>,
    /// Custom command to use the account (e.g. `claude-work`). None for the Default
    /// account (uses the bare `claude`/`codex` command).
    #[serde(default)]
    pub launcher_command: Option<String>,
    /// true for the "Machine default" account pointing at ~/.claude (~/.codex) — read-only.
    #[serde(default)]
    pub is_default: bool,
    /// The account's Google avatar (Antigravity only) — shown instead of the
    /// confusing fingerprint. Computed when building the snapshot, not stored in state.json.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Present when the account runs through an external API/proxy gateway instead of a
    /// subscription login. Such accounts have no quota (the gateway exposes none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_provider: Option<ApiProvider>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolStatus {
    pub id: ToolId,
    pub name: String,
    pub installed: bool,
    /// The account the plain command currently uses (Active state). None = Machine default.
    pub active_account_id: Option<String>,
    pub accounts: Vec<Account>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub tools: Vec<ToolStatus>,
    pub disclaimer_accepted: bool,
    pub auto_switch: bool,
    pub auto_switch_threshold: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddAccountInput {
    pub tool_id: ToolId,
    pub name: String,
    pub mode: AddMode,
    /// Custom command name (e.g. `claude-work`) — required for Claude/Codex (Login mode).
    #[serde(default)]
    pub launcher: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AddMode {
    Import,
    Login,
}

/// Add an account that runs the CLI through an external API/proxy gateway (no OAuth login).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddApiAccountInput {
    pub tool_id: ToolId,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Optional custom command (e.g. `codex-p`). Without one the account is used via the bare
    /// command after pressing Use.
    #[serde(default)]
    pub launcher: Option<String>,
    #[serde(default)]
    pub bypass: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameAccountInput {
    pub tool_id: ToolId,
    pub account_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchAccountInput {
    pub tool_id: ToolId,
    pub account_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLauncherInput {
    pub tool_id: ToolId,
    pub account_id: String,
    pub name: String,
}

// ---------------------------------------------------------------------------
// Token usage tracking (Usage tab) — aggregates token counts + cost from the
// CLIs' local JSONL logs. Claude's logs undercount badly (see usage.rs), so its
// numbers are flagged `estimate: true`; Codex's cumulative token_count is accurate.
// ---------------------------------------------------------------------------

/// A split of tokens by billing category (unified across Claude + Codex).
/// For Codex `cache_creation` is always 0 (it has no prompt-cache-write tier);
/// `input` is the non-cached input (cached input is counted in `cache_read`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBreakdown {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
}

impl TokenBreakdown {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_creation
    }

    pub fn add(&mut self, other: &TokenBreakdown) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_creation += other.cache_creation;
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    /// Local date `YYYY-MM-DD`.
    pub date: String,
    pub tokens: TokenBreakdown,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    pub tokens: TokenBreakdown,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUsage {
    /// Short session id (the JSONL file stem).
    pub id: String,
    /// Local date `YYYY-MM-DD` of the last activity in the session.
    pub date: String,
    pub model: String,
    pub tokens: TokenBreakdown,
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUsage {
    pub tool_id: ToolId,
    pub display_name: String,
    /// true = the numbers are an estimate (Claude's JSONL undercounts tokens).
    pub estimate: bool,
    pub total: TokenBreakdown,
    pub total_cost_usd: Option<f64>,
    pub today: TokenBreakdown,
    pub today_cost_usd: Option<f64>,
    /// Per local-day totals, oldest → newest.
    pub daily: Vec<DayUsage>,
    /// Per-model totals, most tokens first.
    pub by_model: Vec<ModelUsage>,
    /// Recent sessions, newest first (capped).
    pub sessions: Vec<SessionUsage>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub tools: Vec<ToolUsage>,
    pub generated_at: String,
    /// "live" (just fetched), "cached" (LiteLLM cache on disk), or "unavailable".
    pub price_status: String,
    pub price_updated_at: Option<String>,
}

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

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

    /// Short label used in auto-prime log/notification lines (matches the brainstorm wording).
    pub fn prime_label(&self) -> &'static str {
        match self {
            ToolId::Claude => "Claude",
            ToolId::Codex => "Codex",
            ToolId::Antigravity => "Antigravity",
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

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApiGatewayServerState {
    Stopped,
    Running,
    Errored,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApiPoolAccountState {
    Available,
    Exhausted,
    CoolingDown,
    Errored,
    Excluded,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ApiRotationStrategy {
    RoundRobin,
    FillFirst,
}

fn default_api_rotation_strategy() -> ApiRotationStrategy {
    ApiRotationStrategy::RoundRobin
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayKey {
    pub id: String,
    pub name: String,
    /// Only the full secret is persisted locally. It is returned once on create,
    /// and snapshots expose only a masked suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    pub prefix: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    pub created_at: String,
}

/// A combo (9router-style): a named, ordered list of model names. The combo `name` is the model id
/// clients request; each member is just a model string (e.g. `gpt-5-codex`). The provider/account
/// is resolved at request time from the gateway's enabled accounts — a member never names an
/// account. Order is the fallback priority; `strategy` overrides the global default per combo.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayCombo {
    pub id: String,
    /// The model id clients request. Unique. Was `model` in the old pool schema.
    #[serde(alias = "model")]
    pub name: String,
    /// Ordered list of member model names. Old schema stored objects; migrated below.
    #[serde(default, deserialize_with = "deserialize_combo_members")]
    pub members: Vec<String>,
    /// Per-combo rotation strategy. `None` = use the gateway's global strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<ApiRotationStrategy>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

/// Accept both the new shape (array of model-name strings) and the legacy pool shape
/// (array of `{model, ...}` objects), so an existing `state.json` migrates transparently.
fn deserialize_combo_members<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Member {
        Name(String),
        Legacy { model: String },
    }
    let raw = Vec::<Member>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(raw.len());
    for member in raw {
        let model = match member {
            Member::Name(model) => model,
            Member::Legacy { model } => model,
        };
        if !out.contains(&model) {
            out.push(model);
        }
    }
    Ok(out)
}

/// One subscription account's participation in the gateway: whether it may serve API traffic,
/// plus its live rotation state (cooldown/errored). Replaces the per-pool-member state.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayAccount {
    pub tool_id: ToolId,
    pub account_id: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "api_pool_member_default_state")]
    pub state: ApiPoolAccountState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn api_pool_member_default_state() -> ApiPoolAccountState {
    ApiPoolAccountState::Available
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayModelRegistry {
    pub tool_id: ToolId,
    pub account_id: String,
    #[serde(default)]
    pub models: Vec<String>,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayConfig {
    #[serde(default = "default_api_bind_host")]
    pub bind_host: String,
    #[serde(default = "default_api_port")]
    pub port: u16,
    #[serde(default = "default_api_quota_threshold")]
    pub quota_threshold: f64,
    #[serde(default = "default_api_max_retries")]
    pub max_retries: u8,
    #[serde(default = "default_api_rotation_strategy")]
    pub rotation_strategy: ApiRotationStrategy,
    #[serde(default)]
    pub keys: Vec<ApiGatewayKey>,
    /// Combos (named model lists). Reads the legacy `pools` key too for migration.
    #[serde(default, alias = "pools")]
    pub combos: Vec<ApiGatewayCombo>,
    /// Which subscription accounts may serve gateway traffic, plus their rotation state.
    #[serde(default)]
    pub accounts: Vec<ApiGatewayAccount>,
    #[serde(default)]
    pub model_registry: Vec<ApiGatewayModelRegistry>,
    #[serde(default)]
    pub virtual_claude_enabled: bool,
    #[serde(default)]
    pub virtual_codex_enabled: bool,
}

impl Default for ApiGatewayConfig {
    fn default() -> Self {
        Self {
            bind_host: default_api_bind_host(),
            port: default_api_port(),
            quota_threshold: default_api_quota_threshold(),
            max_retries: default_api_max_retries(),
            rotation_strategy: default_api_rotation_strategy(),
            keys: Vec::new(),
            combos: Vec::new(),
            accounts: Vec::new(),
            model_registry: Vec::new(),
            virtual_claude_enabled: false,
            virtual_codex_enabled: false,
        }
    }
}

fn default_api_bind_host() -> String {
    "127.0.0.1".to_string()
}

fn default_api_port() -> u16 {
    8783
}

fn default_api_quota_threshold() -> f64 {
    95.0
}

fn default_api_max_retries() -> u8 {
    3
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewayStatus {
    pub state: ApiGatewayServerState,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiGatewaySnapshot {
    pub config: ApiGatewayConfig,
    pub status: ApiGatewayStatus,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiUsageReport {
    pub generated_at: String,
    pub total_requests: u64,
    pub total: TokenBreakdown,
    pub rows: Vec<ApiUsageRow>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiUsageRow {
    #[serde(alias = "poolModel")]
    pub combo_name: String,
    pub key_id: String,
    pub account_id: String,
    pub tool_id: ToolId,
    pub requests: u64,
    pub tokens: TokenBreakdown,
    pub last_used_at: String,
}

impl Default for ApiGatewayStatus {
    fn default() -> Self {
        Self {
            state: ApiGatewayServerState::Stopped,
            base_url: "http://127.0.0.1:8783".to_string(),
            error: None,
        }
    }
}

impl Default for ApiGatewaySnapshot {
    fn default() -> Self {
        Self {
            config: ApiGatewayConfig::default(),
            status: ApiGatewayStatus::default(),
        }
    }
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
    /// Subscription plan label parsed from the usage API (e.g. "Plus", "Pro", "Max").
    /// None when the API doesn't report one. Shown as a small badge next to the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub updated_at: Option<String>,
    pub error: Option<String>,
}

impl QuotaInfo {
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
            plan: None,
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
    /// Legacy global switch fields kept for older UI/dev snapshots.
    pub auto_switch: bool,
    pub auto_switch_threshold: f64,
    #[serde(default)]
    pub auto_switch_settings: std::collections::BTreeMap<String, AutoSwitchSetting>,
    /// Per-account auto session prime schedules, keyed by account id.
    #[serde(default)]
    pub auto_prime: std::collections::BTreeMap<String, AutoPrimeSetting>,
    #[serde(default)]
    pub tool_setups: std::collections::BTreeMap<String, ToolSetup>,
    #[serde(default)]
    pub api_gateway: ApiGatewaySnapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoSwitchSetting {
    pub enabled: bool,
    pub threshold: f64,
}

impl Default for AutoSwitchSetting {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: 100.0,
        }
    }
}

/// Per-account "auto session prime" config. Keyed by account id in `StoredState.auto_prime`.
/// At the scheduled `time` (machine local), the app sends a minimal "hi" to start a fresh
/// 5-hour window, so the reset clock is anchored to the user's work rhythm. Each account
/// primes at most once per day for a given `time` (the `last_primed_*` fields enforce this).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoPrimeSetting {
    /// Whether scheduled priming is on for this account.
    pub enabled: bool,
    /// The single daily prime time, `HH:MM` 24h, in the machine's local timezone.
    pub time: String,
    /// Local date (`YYYY-MM-DD`) the account was last primed — guards "once per day".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_primed_date: Option<String>,
    /// The `HH:MM` that was primed on `last_primed_date`. A new time differing from this
    /// is allowed to prime again the same day (changing the schedule is not a re-run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_primed_time: Option<String>,
    /// Short status of the most recent prime attempt: "success" | "failed" | "skip" | "hold".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    /// ISO timestamp of the most recent prime attempt (any outcome).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    /// On-demand extend (mechanism 2): set true when the user accepts the "extend?" prompt.
    /// The next time the current 5h window ends, the account is primed once, then this clears.
    #[serde(default)]
    pub extend_requested: bool,
    /// Local date (`YYYY-MM-DD`) the user was last reminded the window is about to end, so the
    /// app prompts at most once per window-ending per day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_reminded_reset: Option<String>,
    /// When true, the app auto-extends without asking: as the window nears its end it sets
    /// `extend_requested` itself instead of prompting. Default false (the brainstorm default is
    /// to ASK). A convenience for days the user doesn't want to confirm each time.
    #[serde(default)]
    pub auto_extend: bool,
    /// When a prime is held (old window still active), the scheduler skips this account until this
    /// ISO instant (= reset_at + 5min), so it doesn't re-attempt + re-log "HOÃN" every minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_until: Option<String>,
    /// The reset_at the user explicitly dismissed the "extend?" prompt for, so the UI hides the
    /// button and the poller doesn't re-prompt for that same window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_dismissed_reset: Option<String>,
}

impl Default for AutoPrimeSetting {
    fn default() -> Self {
        Self {
            enabled: false,
            time: "05:30".to_string(),
            last_primed_date: None,
            last_primed_time: None,
            last_result: None,
            last_attempt_at: None,
            extend_requested: false,
            extend_reminded_reset: None,
            auto_extend: false,
            deferred_until: None,
            extend_dismissed_reset: None,
        }
    }
}

/// One day's tally of auto-prime outcomes, parsed from the activity log for the stats view.
#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutoPrimeDayStat {
    /// Local date `YYYY-MM-DD`.
    pub date: String,
    pub success: u32,
    pub failed: u32,
    pub hold: u32,
    pub skip: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutoPrimeInput {
    pub tool_id: ToolId,
    pub account_id: String,
    pub enabled: bool,
    /// `HH:MM` 24h local time.
    pub time: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutoPrimeAllInput {
    /// `HH:MM` 24h applied to every prime-eligible (subscription) account.
    pub time: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmExtendInput {
    pub tool_id: ToolId,
    pub account_id: String,
    /// true = user accepted "extend?"; false = user dismissed it.
    pub accept: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAutoExtendInput {
    pub tool_id: ToolId,
    pub account_id: String,
    /// true = auto-extend without asking; false = ask each time (default).
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DetectionSource {
    Env,
    Default,
    Path,
    AppManaged,
    Manual,
    Fallback,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSetup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_config_dir: Option<PathBuf>,
    pub binary_source: DetectionSource,
    pub config_source: DetectionSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validated_at: Option<String>,
    #[serde(default)]
    pub validation_warnings: Vec<String>,
}

impl Default for ToolSetup {
    fn default() -> Self {
        Self {
            binary_path: None,
            default_config_dir: None,
            binary_source: DetectionSource::Fallback,
            config_source: DetectionSource::Fallback,
            validated_at: None,
            validation_warnings: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationEvidence {
    pub label: String,
    pub found: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigCandidate {
    pub path: PathBuf,
    pub source: DetectionSource,
    pub score: u32,
    pub valid: bool,
    pub is_app_managed: bool,
    pub evidence: Vec<ValidationEvidence>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BinaryCandidate {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<PathBuf>,
    pub source: DetectionSource,
    pub score: u32,
    pub valid: bool,
    pub is_app_launcher: bool,
    pub evidence: Vec<ValidationEvidence>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ResolutionKind {
    Resolved,
    NeedsUserChoice,
    NeedsManualInput,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionResolution {
    pub kind: ResolutionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<ToolSetup>,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionReport {
    pub tool_id: ToolId,
    pub config_candidates: Vec<ConfigCandidate>,
    pub binary_candidates: Vec<BinaryCandidate>,
    pub resolution: DetectionResolution,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetToolSetupInput {
    pub tool_id: ToolId,
    pub binary_path: PathBuf,
    pub default_config_dir: PathBuf,
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
pub struct StartApiGatewayInput {
    pub bind_host: String,
    pub port: u16,
    pub quota_threshold: f64,
    #[serde(default = "default_api_rotation_strategy")]
    pub rotation_strategy: ApiRotationStrategy,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiGatewayKeyInput {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiGatewayKeyResult {
    pub snapshot: AppSnapshot,
    pub secret: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveApiGatewayComboInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    /// Ordered list of member model names.
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<ApiRotationStrategy>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteApiGatewayKeyInput {
    pub key_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteApiGatewayComboInput {
    pub combo_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetApiGatewayAccountInput {
    pub tool_id: ToolId,
    pub account_id: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVirtualApiAccountInput {
    pub tool_id: ToolId,
    /// Combo (model id) to bind the virtual account to. None = first enabled combo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
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

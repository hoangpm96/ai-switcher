use crate::models::{
    Account, AccountState, ApiGatewayConfig, AutoSwitchSetting, OverlaySettings, ToolId, ToolSetup,
};
use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredState {
    pub disclaimer_accepted: bool,
    pub accounts: Vec<Account>,
    /// Auto-switch the account in use (plain command) when it hits the quota threshold.
    #[serde(default)]
    pub auto_switch: bool,
    /// The % used that triggers auto-switch (default 100 = fully exhausted).
    #[serde(default = "default_threshold")]
    pub auto_switch_threshold: f64,
    /// Per-tool auto-switch settings. Claude/Codex are independent; Antigravity is not supported.
    #[serde(default)]
    pub auto_switch_settings: BTreeMap<String, AutoSwitchSetting>,
    /// Resolved CLI binary/config dirs per tool. Missing = detect on startup / ask user.
    #[serde(default)]
    pub tool_setups: BTreeMap<String, ToolSetup>,
    /// Local OpenAI/Anthropic-compatible proxy settings for the API tab.
    #[serde(default)]
    pub api_gateway: ApiGatewayConfig,
    /// Always-on-top quota overlay window (which accounts, size/position, opacity).
    #[serde(default)]
    pub overlay: OverlaySettings,
    /// Claude organization uuid → who it is (email, app account names). Kept when an account is
    /// deleted so the Usage tab can still name usage of removed accounts.
    #[serde(default)]
    pub claude_orgs: BTreeMap<String, crate::models::ClaudeOrgRecord>,
}

fn default_threshold() -> f64 {
    100.0
}

impl Default for StoredState {
    fn default() -> Self {
        Self {
            disclaimer_accepted: false,
            accounts: Vec::new(),
            auto_switch: false,
            auto_switch_threshold: default_threshold(),
            auto_switch_settings: BTreeMap::new(),
            tool_setups: BTreeMap::new(),
            api_gateway: ApiGatewayConfig::default(),
            overlay: OverlaySettings::default(),
            claude_orgs: BTreeMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new() -> Result<Self> {
        let dirs = ProjectDirs::from("dev", "hoangphan", "AI Account Switcher")
            .context("Couldn't determine the app data directory")?;
        let root = dirs.data_local_dir().to_path_buf();
        fs::create_dir_all(root.join("accounts"))?;
        fs::create_dir_all(root.join("backups"))?;
        // Best-effort cleanup of files the removed auto-session-prime feature left behind. Deleting
        // `wake-request.txt` also makes a still-installed legacy wake-helper daemon cancel any pmset
        // wake it had armed (its WatchPaths script clears the previous wake when the file goes away).
        for stale in ["prime-runtime.json", "prime.lock", "wake-request.txt"] {
            let _ = fs::remove_file(root.join(stale));
        }
        let _ = fs::remove_dir_all(root.join("prime-claims"));
        Ok(Self { root })
    }

    pub fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// Incremental token-usage cache (per-file cursors + aggregated buckets).
    pub fn usage_cache_path(&self) -> PathBuf {
        self.root.join("usage.json")
    }

    /// Cached copy of LiteLLM's pricing dataset (refreshed at most daily).
    pub fn price_cache_path(&self) -> PathBuf {
        self.root.join("litellm_prices.json")
    }

    /// Usage produced by the local API gateway only. Kept separate from `usage.json`
    /// because CLI JSONL scans may also see some proxy-originated requests.
    pub fn api_usage_path(&self) -> PathBuf {
        self.root.join("api_usage.json")
    }

    /// Human-readable activity log for "Prime ngay" attempts (one line per event). The filename is
    /// kept from the removed auto-prime feature so the user's existing log history stays in place.
    pub fn auto_prime_log_path(&self) -> PathBuf {
        self.root.join("auto-prime.log")
    }

    /// The tool's accounts root (`accounts/<tool>/`), holding one dir per profile account.
    pub fn tool_accounts_root(&self, tool_id: &ToolId) -> PathBuf {
        self.root.join("accounts").join(tool_id.as_str())
    }

    pub fn account_dir(&self, tool_id: &ToolId, account_id: &str) -> PathBuf {
        self.root
            .join("accounts")
            .join(tool_id.as_str())
            .join(account_id)
    }

    pub fn active_profile_path(&self, tool_id: &ToolId) -> PathBuf {
        self.root
            .join("active")
            .join(format!("{}.profile", tool_id.as_str()))
    }

    pub fn load(&self) -> Result<StoredState> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(StoredState::default());
        }
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, state: &StoredState) -> Result<()> {
        let path = self.state_path();
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn for_test(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("accounts"))?;
        fs::create_dir_all(root.join("backups"))?;
        Ok(Self { root })
    }
}

pub fn normalize_account_states(
    accounts: &mut [Account],
    tool_id: &ToolId,
    active_id: Option<&str>,
) {
    for account in accounts.iter_mut().filter(|item| &item.tool_id == tool_id) {
        account.state = if Some(account.id.as_str()) == active_id {
            if account.state == AccountState::Exhausted {
                AccountState::Exhausted
            } else {
                AccountState::Active
            }
        } else if account.state == AccountState::Active {
            AccountState::Idle
        } else {
            account.state.clone()
        };
    }
}

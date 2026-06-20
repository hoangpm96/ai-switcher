use crate::models::{
    Account, AccountState, ApiGatewayConfig, AutoPrimeSetting, AutoSwitchSetting,
    PrimeRuntimeState, ToolId, ToolSetup,
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
    /// Per-account "auto session prime" schedules, keyed by account id. Only subscription
    /// (OAuth) Claude/Codex accounts are eligible — API-proxy accounts have no 5h window.
    #[serde(default)]
    pub auto_prime: BTreeMap<String, AutoPrimeSetting>,
    /// Resolved CLI binary/config dirs per tool. Missing = detect on startup / ask user.
    #[serde(default)]
    pub tool_setups: BTreeMap<String, ToolSetup>,
    /// Local OpenAI/Anthropic-compatible proxy settings for the API tab.
    #[serde(default)]
    pub api_gateway: ApiGatewayConfig,
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
            auto_prime: BTreeMap::new(),
            tool_setups: BTreeMap::new(),
            api_gateway: ApiGatewayConfig::default(),
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
        fs::create_dir_all(root.join("prime-claims"))?;
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

    /// Human-readable activity log for auto session priming (one line per event).
    pub fn auto_prime_log_path(&self) -> PathBuf {
        self.root.join("auto-prime.log")
    }

    pub fn prime_runtime_path(&self) -> PathBuf {
        self.root.join("prime-runtime.json")
    }

    /// Advisory cross-process lock used to serialize GUI and headless prime batches.
    pub fn prime_lock_path(&self) -> PathBuf {
        self.root.join("prime.lock")
    }

    pub fn prime_claim_path(&self, key: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(key.as_bytes());
        self.root
            .join("prime-claims")
            .join(format!("{digest:x}.claim"))
    }

    /// Remove old, unreferenced claim markers. Claims intentionally survive terminal attempts to
    /// protect against stale cross-process state, but they need not accumulate forever.
    pub fn gc_prime_claims(&self, runtime: &PrimeRuntimeState) {
        let referenced: std::collections::BTreeSet<PathBuf> = runtime
            .attempts
            .values()
            .filter_map(|attempt| attempt.claim_key.as_deref())
            .map(|key| self.prime_claim_path(key))
            .collect();
        let Ok(entries) = fs::read_dir(self.root.join("prime-claims")) else {
            return;
        };
        let max_age = std::time::Duration::from_secs(48 * 60 * 60);
        for entry in entries.flatten() {
            let path = entry.path();
            if referenced.contains(&path) {
                continue;
            }
            let old = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age > max_age);
            if old {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// The root data dir (used to place the pmset wake helper's request/script files).
    pub fn root_dir(&self) -> &std::path::Path {
        &self.root
    }

    /// File the app writes the next desired wake time into; the root LaunchDaemon helper watches
    /// it and runs `pmset schedule wake`. Lives in the data dir so the unprivileged app can write it.
    pub fn wake_request_path(&self) -> PathBuf {
        self.root.join("wake-request.txt")
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

    pub fn load_prime_runtime(&self) -> Result<PrimeRuntimeState> {
        let path = self.prime_runtime_path();
        if !path.exists() {
            return Ok(PrimeRuntimeState::default());
        }
        let text = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save_prime_runtime(&self, runtime: &PrimeRuntimeState) -> Result<()> {
        let path = self.prime_runtime_path();
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(runtime)?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    #[cfg(test)]
    pub fn for_test(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(root.join("accounts"))?;
        fs::create_dir_all(root.join("backups"))?;
        fs::create_dir_all(root.join("prime-claims"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{PendingPrimeAttempt, PrimeAttemptPhase, PrimeRuntimeState};

    #[test]
    fn prime_runtime_round_trips_atomically() {
        let root = std::env::temp_dir().join(format!(
            "ai-switcher-prime-runtime-{}",
            uuid::Uuid::new_v4()
        ));
        let store = Store::for_test(root.clone()).unwrap();
        let mut runtime = PrimeRuntimeState::default();
        runtime.attempts.insert(
            "account-1".to_string(),
            PendingPrimeAttempt {
                version: 1,
                account_id: "account-1".to_string(),
                tool_id: ToolId::Codex,
                consumes_scheduled_slot: true,
                resolves_extend: false,
                manual: false,
                scheduled_date: Some("2026-06-20".to_string()),
                scheduled_time: Some("07:00".to_string()),
                anchor_at: "2026-06-20T00:00:00Z".to_string(),
                deadline_at: "2026-06-20T00:45:00Z".to_string(),
                phase: PrimeAttemptPhase::WaitingRetry,
                next_action_at: "2026-06-20T00:05:00Z".to_string(),
                send_attempts: 1,
                last_send_at: Some("2026-06-20T00:00:00Z".to_string()),
                baseline_reset_at: None,
                last_observation: None,
                last_error: Some("rolling".to_string()),
                claim_key: Some("scheduled|account-1|2026-06-20|07:00".to_string()),
            },
        );
        store.save_prime_runtime(&runtime).unwrap();
        let loaded = store.load_prime_runtime().unwrap();
        assert_eq!(loaded.attempts.len(), 1);
        assert_eq!(
            loaded.attempts["account-1"].phase,
            PrimeAttemptPhase::WaitingRetry
        );
        assert!(!store
            .prime_runtime_path()
            .with_extension("json.tmp")
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }
}

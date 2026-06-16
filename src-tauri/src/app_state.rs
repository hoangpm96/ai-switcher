use crate::models::{
    Account, AccountState, AddAccountInput, AddApiAccountInput, ApiGatewayAccount, ApiGatewayCombo,
    ApiGatewayConfig, ApiGatewayKey, ApiGatewayServerState, ApiGatewaySnapshot, ApiProvider,
    ApiUsageReport, AppSnapshot, AutoSwitchSetting, CreateApiGatewayKeyInput,
    CreateApiGatewayKeyResult, CreateVirtualApiAccountInput, DeleteApiGatewayComboInput,
    DeleteApiGatewayKeyInput, DetectionReport, QuotaInfo, RenameAccountInput,
    SaveApiGatewayComboInput, SetApiGatewayAccountInput, SetLauncherInput, SetToolSetupInput,
    StartApiGatewayInput, SwitchAccountInput, ToolId, ToolStatus, UsageReport,
};
use crate::quota::read_quota;
use crate::store::{normalize_account_states, Store, StoredState};
use crate::tools::{
    antigravity_capture, antigravity_current_token, antigravity_new_login, antigravity_open_ide,
    antigravity_quit_ide, antigravity_restore, antigravity_saved_profile, antigravity_saved_token,
    clear_active_profile, create_profile_with_default, default_config_dir, delete_account_files,
    full_launcher_name, install_shell_hook, is_installed, launch_profile_login,
    launcher_name_collides_with_system, link_shared_config_to, link_shared_sessions_to,
    remove_launcher, write_active_profile, write_api_key_file, write_api_launcher,
    write_claude_proxy_settings, write_codex_proxy_config, write_launcher,
};
use anyhow::{Context, Result};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

pub struct ManagedState {
    pub store: Store,
    pub data: Mutex<StoredState>,
    pub api_server: Mutex<crate::api_gateway::ApiServerHandle>,
    /// True while a prime batch is running, so the per-minute scheduler tick doesn't start a
    /// second overlapping batch (a single attempt can block for minutes on send retries).
    pub priming: std::sync::atomic::AtomicBool,
}

impl ManagedState {
    pub fn new() -> Result<Self> {
        let store = Store::new()?;
        let mut data = store.load()?;
        migrate_defaults(&mut data.accounts);
        migrate_auto_switch_settings(&mut data);
        autodetect_missing_tool_setups(&store, &mut data);
        store.save(&data)?;
        let server = crate::api_gateway::ApiServerHandle::stopped(&data.api_gateway);
        let managed = Self {
            store,
            data: Mutex::new(data),
            api_server: Mutex::new(server),
            priming: std::sync::atomic::AtomicBool::new(false),
        };
        // Clean up orphan active files: pointing to a deleted profile → clear + reinstall the hook.
        managed.heal_active_profiles();
        // The API server never auto-starts. Recover from a crash/forced quit that may have left
        // the bare CLI command pointing at a virtual account whose endpoint is now offline.
        let _ = managed.deactivate_virtual_api_accounts();
        Ok(managed)
    }

    /// For each CLI tool: if the active file points to a profile that belongs to NO account
    /// in the state (the account was deleted — the dir may be recreated by the CLI itself, so we
    /// can't rely on the dir's existence) → clear it so the plain command falls back to the
    /// machine Default. Also clean up orphan profile dirs on disk.
    fn heal_active_profiles(&self) {
        let data = match self.data.lock() {
            Ok(data) => data,
            Err(_) => return,
        };
        let mut changed = false;
        for tool_id in [ToolId::Claude, ToolId::Codex] {
            let valid_dirs: Vec<std::path::PathBuf> = data
                .accounts
                .iter()
                .filter(|a| a.tool_id == tool_id && !a.is_default)
                .map(|a| self.store.account_dir(&tool_id, &a.id))
                .collect();

            // Seed the onboarding flag + link the shared session for every profile (idempotent)
            // — ensures accounts logged in from a previous session also skip the wizard and
            // share the session store with Default.
            for account in data
                .accounts
                .iter()
                .filter(|a| a.tool_id == tool_id && !a.is_default)
            {
                let dir = self.store.account_dir(&tool_id, &account.id);
                crate::tools::seed_onboarding(&tool_id, &dir);
                if let Some(default_dir) = configured_default_config_dir(&data, &tool_id) {
                    link_shared_sessions_to(&tool_id, &dir, &default_dir);
                    if account.api_provider.is_none() {
                        link_shared_config_to(&tool_id, &dir, &default_dir);
                    }
                }
            }

            // Clear active if it points to a profile that belongs to no account.
            let active = self.store.active_profile_path(&tool_id);
            if let Ok(target) = std::fs::read_to_string(&active) {
                let target = target.trim();
                if !target.is_empty() && !valid_dirs.iter().any(|d| d.to_string_lossy() == target) {
                    let _ = clear_active_profile(&tool_id, &self.store);
                    changed = true;
                }
            }

            // Clean up orphan profile dirs (not managed by any account).
            let tool_root = self.store.account_dir(&tool_id, "");
            if let Ok(entries) = std::fs::read_dir(&tool_root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && !valid_dirs.iter().any(|d| *d == path) {
                        let _ = std::fs::remove_dir_all(&path);
                    }
                }
            }
        }
        if changed {
            let _ = install_shell_hook(&self.store);
        }
    }

    pub fn snapshot(&self) -> Result<AppSnapshot> {
        let data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        let snapshot_data = self.store.load().unwrap_or_else(|_| data.clone());
        let status = self
            .api_server
            .lock()
            .map(|server| server.status.clone())
            .unwrap_or_else(|_| {
                crate::api_gateway::ApiServerHandle::stopped(&snapshot_data.api_gateway).status
            });
        Ok(build_snapshot(&self.store, &snapshot_data, status))
    }

    pub fn start_api_gateway(&self, input: StartApiGatewayInput) -> Result<AppSnapshot> {
        // NOTE: do NOT refresh the model registry here. Discovery spawns a Codex subprocess and
        // makes blocking HTTP calls per account — doing that inline froze Start (and could hang
        // the whole app). The gateway serves fine without a fresh registry (name heuristics +
        // the cached registry). The UI kicks off a background refresh after Start succeeds.
        let bind_host = input.bind_host.trim();
        if !matches!(bind_host, "127.0.0.1" | "0.0.0.0") {
            anyhow::bail!("API server bind address must be 127.0.0.1 or 0.0.0.0");
        }
        if input.port == 0 {
            anyhow::bail!("API server port must be between 1 and 65535");
        }
        let config = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.api_gateway.bind_host = bind_host.to_string();
            data.api_gateway.port = input.port;
            data.api_gateway.quota_threshold = input.quota_threshold.clamp(50.0, 100.0);
            data.api_gateway.rotation_strategy = input.rotation_strategy;
            self.store.save(&data)?;
            data.api_gateway.clone()
        };
        let mut server = self
            .api_server
            .lock()
            .map_err(|_| anyhow::anyhow!("API server lock poisoned"))?;
        server.stop(&config);
        let handle = crate::api_gateway::start_server(self.store.clone(), config.clone());
        *server = match handle {
            Ok(handle) => handle,
            Err(err) => crate::api_gateway::ApiServerHandle {
                shutdown: None,
                thread: None,
                status: crate::models::ApiGatewayStatus {
                    state: ApiGatewayServerState::Errored,
                    base_url: crate::api_gateway::base_url(&config),
                    error: Some(err.to_string()),
                },
            },
        };
        let errored = server.status.state == ApiGatewayServerState::Errored;
        drop(server);
        if errored {
            self.deactivate_virtual_api_accounts()?;
        }
        self.snapshot()
    }

    pub fn stop_api_gateway(&self) -> Result<AppSnapshot> {
        let config = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.api_gateway.clone()
        };
        self.api_server
            .lock()
            .map_err(|_| anyhow::anyhow!("API server lock poisoned"))?
            .stop(&config);
        self.deactivate_virtual_api_accounts()?;
        self.snapshot()
    }

    pub fn create_virtual_api_account(
        &self,
        input: CreateVirtualApiAccountInput,
    ) -> Result<AppSnapshot> {
        if !matches!(input.tool_id, ToolId::Claude | ToolId::Codex) {
            anyhow::bail!("Local API accounts are only supported for Claude Code and Codex");
        }
        let running = self
            .api_server
            .lock()
            .map_err(|_| anyhow::anyhow!("API server lock poisoned"))?
            .status
            .state
            == ApiGatewayServerState::Running;
        if !running {
            anyhow::bail!("Start the local API gateway before adding a local API account");
        }
        let name = virtual_api_name(&input.tool_id).to_string();
        let (id, base_url, api_key, model, default_dir, is_new) = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let existing_id = data
                .accounts
                .iter()
                .find(|account| account.tool_id == input.tool_id && account.name == name)
                .map(|account| account.id.clone());
            let api_key = match data
                .api_gateway
                .keys
                .iter()
                .find_map(|key| key.secret.clone())
            {
                Some(secret) => secret,
                None => {
                    let secret = generate_api_key();
                    data.api_gateway.keys.push(ApiGatewayKey {
                        id: Uuid::new_v4().to_string(),
                        name: "Local CLI".to_string(),
                        prefix: mask_key(&secret),
                        secret: Some(secret.clone()),
                        enabled: true,
                        expires_at: None,
                        created_at: now(),
                    });
                    secret
                }
            };
            // Bind to the requested model if given — a combo name OR any model the gateway can
            // serve directly. Else fall back to the first enabled combo.
            let model = match input.model.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                Some(requested) => {
                    if !crate::api_gateway::model_is_servable(&data, requested) {
                        anyhow::bail!(
                            "'{requested}' isn't a combo or a model your enabled accounts serve"
                        );
                    }
                    requested.to_string()
                }
                None => data
                    .api_gateway
                    .combos
                    .iter()
                    .find(|combo| combo.enabled)
                    .map(|combo| combo.name.clone())
                    .context("Create at least one combo or pick a model before adding the account")?,
            };
            let base_url = crate::api_gateway::base_url(&data.api_gateway);
            let default_dir = configured_default_config_dir(&data, &input.tool_id)
                .context("CLI setup is ambiguous — choose the tool's default config first")?;
            self.store.save(&data)?;
            let is_new = existing_id.is_none();
            (
                existing_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                base_url,
                api_key,
                model,
                default_dir,
                is_new,
            )
        };

        let profile = create_profile_with_default(&input.tool_id, &self.store, &id, &default_dir)?;
        match input.tool_id {
            ToolId::Codex => {
                write_codex_proxy_config(
                    &profile,
                    &name,
                    &format!("{}/v1", base_url.trim_end_matches('/')),
                    &model,
                )?;
                write_api_key_file(&profile, &api_key)?;
            }
            ToolId::Claude => {
                write_claude_proxy_settings(&profile, &base_url, &api_key, &model)?;
                crate::tools::seed_onboarding(&input.tool_id, &profile);
            }
            ToolId::Antigravity => unreachable!("guarded above"),
        }
        // Give the virtual account its own standalone command (`claude-api` / `codex-api`) so it can
        // be run in parallel from any terminal without "Use"-ing it as the active account. Failure
        // to write the launcher is non-fatal — the account still works via "Use".
        let launcher = {
            let binary = {
                let data = self
                    .data
                    .lock()
                    .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
                configured_binary_path(&data, &input.tool_id)
            };
            let full = full_launcher_name(&input.tool_id, &name).ok();
            match (full, binary) {
                (Some(full), Some(binary)) => write_api_launcher(
                    &input.tool_id,
                    &self.store,
                    &id,
                    &full,
                    &model,
                    false,
                    &binary,
                )
                .ok()
                .map(|_| full),
                _ => None,
            }
        };
        let timestamp = now();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            match input.tool_id {
                ToolId::Claude => data.api_gateway.virtual_claude_enabled = true,
                ToolId::Codex => data.api_gateway.virtual_codex_enabled = true,
                ToolId::Antigravity => {}
            }
            if is_new {
                data.accounts.push(Account {
                    id,
                    tool_id: input.tool_id.clone(),
                    name,
                    state: AccountState::Idle,
                    fingerprint: "api-local".to_string(),
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                    last_used_at: None,
                    quota: None,
                    launcher_command: launcher,
                    is_default: false,
                    avatar_url: None,
                    api_provider: Some(ApiProvider {
                        base_url,
                        model,
                        bypass: false,
                    }),
                });
            } else if let Some(account) = data
                .accounts
                .iter_mut()
                .find(|account| account.tool_id == input.tool_id && account.name == name)
            {
                account.state = if account.state == AccountState::Active {
                    AccountState::Active
                } else {
                    AccountState::Idle
                };
                account.fingerprint = "api-local".to_string();
                account.quota = None;
                account.launcher_command = launcher;
                account.updated_at = timestamp;
                account.api_provider = Some(ApiProvider {
                    base_url,
                    model,
                    bypass: false,
                });
            }
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    fn deactivate_virtual_api_accounts(&self) -> Result<()> {
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        for tool_id in [ToolId::Claude, ToolId::Codex] {
            let active_target = std::fs::read_to_string(self.store.active_profile_path(&tool_id))
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            let active_virtual_id = active_target.as_deref().and_then(|target| {
                data.accounts
                    .iter()
                    .find(|account| {
                        account.tool_id == tool_id
                            && is_virtual_api_account(account)
                            && self
                                .store
                                .account_dir(&tool_id, &account.id)
                                .to_string_lossy()
                                == target
                    })
                    .map(|account| account.id.clone())
            });
            if active_virtual_id.is_some() {
                let _ = clear_active_profile(&tool_id, &self.store);
                let default_id = data
                    .accounts
                    .iter()
                    .find(|account| account.tool_id == tool_id && account.is_default)
                    .map(|account| account.id.clone());
                normalize_account_states(&mut data.accounts, &tool_id, default_id.as_deref());
            }
            for account in data
                .accounts
                .iter_mut()
                .filter(|account| account.tool_id == tool_id && is_virtual_api_account(account))
            {
                if account.state == AccountState::Active {
                    account.state = AccountState::Idle;
                    account.updated_at = now();
                }
            }
        }
        data.api_gateway.virtual_claude_enabled = false;
        data.api_gateway.virtual_codex_enabled = false;
        self.store.save(&data)?;
        let _ = install_shell_hook(&self.store);
        Ok(())
    }

    pub fn create_api_gateway_key(
        &self,
        input: CreateApiGatewayKeyInput,
    ) -> Result<CreateApiGatewayKeyResult> {
        let secret = generate_api_key();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.api_gateway.keys.push(ApiGatewayKey {
                id: Uuid::new_v4().to_string(),
                name: input.name.trim().chars().take(40).collect(),
                prefix: mask_key(&secret),
                secret: Some(secret.clone()),
                enabled: true,
                expires_at: input.expires_at,
                created_at: now(),
            });
            self.store.save(&data)?;
        }
        Ok(CreateApiGatewayKeyResult {
            snapshot: self.snapshot()?,
            secret,
        })
    }

    pub fn delete_api_gateway_key(&self, input: DeleteApiGatewayKeyInput) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.api_gateway.keys.retain(|key| key.id != input.key_id);
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Return the full secret for a stored key so the UI can copy it on demand. Snapshots redact
    /// secrets, so this is the only way to recover a previously-created key (local single-user app).
    pub fn reveal_api_gateway_key(&self, key_id: String) -> Result<String> {
        let data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        data.api_gateway
            .keys
            .iter()
            .find(|key| key.id == key_id)
            .and_then(|key| key.secret.clone())
            .context("Key not found")
    }

    pub fn save_api_gateway_combo(&self, input: SaveApiGatewayComboInput) -> Result<AppSnapshot> {
        let name = input.name.trim();
        if name.is_empty() {
            anyhow::bail!("Combo name is required");
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            anyhow::bail!("Combo name allows only letters, numbers, '-', '_' and '.'");
        }
        // De-dupe member models, preserving order; drop blanks.
        let mut seen = std::collections::HashSet::new();
        let members: Vec<String> = input
            .members
            .iter()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty() && seen.insert(model.clone()))
            .collect();
        if members.is_empty() {
            anyhow::bail!("A combo must include at least one model");
        }
        let timestamp = now();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            if data
                .api_gateway
                .combos
                .iter()
                .any(|combo| combo.name == name && Some(combo.id.as_str()) != input.id.as_deref())
            {
                anyhow::bail!("Combo name must be unique");
            }
            let existing = input
                .id
                .as_deref()
                .and_then(|id| data.api_gateway.combos.iter().find(|combo| combo.id == id));
            let combo = ApiGatewayCombo {
                id: input
                    .id
                    .clone()
                    .unwrap_or_else(|| Uuid::new_v4().to_string()),
                name: name.to_string(),
                members,
                strategy: input.strategy,
                enabled: existing.is_none_or(|combo| combo.enabled),
                created_at: existing.map_or_else(|| timestamp.clone(), |c| c.created_at.clone()),
                updated_at: timestamp,
            };
            match data
                .api_gateway
                .combos
                .iter()
                .position(|existing| existing.id == combo.id)
            {
                Some(index) => data.api_gateway.combos[index] = combo,
                None => data.api_gateway.combos.push(combo),
            }
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    pub fn delete_api_gateway_combo(
        &self,
        input: DeleteApiGatewayComboInput,
    ) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.api_gateway
                .combos
                .retain(|combo| combo.id != input.combo_id);
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Toggle whether a subscription account participates in gateway rotation. Upserts the
    /// participation entry (missing = enabled by default).
    pub fn set_api_gateway_account(
        &self,
        input: SetApiGatewayAccountInput,
    ) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            match data
                .api_gateway
                .accounts
                .iter_mut()
                .find(|entry| entry.tool_id == input.tool_id && entry.account_id == input.account_id)
            {
                Some(entry) => entry.enabled = input.enabled,
                None => data.api_gateway.accounts.push(ApiGatewayAccount {
                    tool_id: input.tool_id,
                    account_id: input.account_id,
                    enabled: input.enabled,
                    state: crate::models::ApiPoolAccountState::Available,
                    cooldown_until: None,
                    error: None,
                }),
            }
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    pub fn refresh_api_gateway_models(&self) -> Result<AppSnapshot> {
        let (data, accounts) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let accounts = data
                .accounts
                .iter()
                .filter(|account| {
                    matches!(account.tool_id, ToolId::Claude | ToolId::Codex)
                        && account.api_provider.is_none()
                        && account.state != AccountState::NeedsLogin
                })
                .cloned()
                .collect::<Vec<_>>();
            (data.clone(), accounts)
        };
        let registry = accounts
            .iter()
            .map(|account| {
                crate::api_gateway::discover_account_models(
                    &self.store,
                    &data,
                    account,
                    configured_binary_path(&data, &account.tool_id).as_deref(),
                )
            })
            .collect();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.api_gateway.model_registry = registry;
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    pub fn detect_tool_setup(&self, tool_id: ToolId) -> DetectionReport {
        crate::detection::detect_tool_setup(&tool_id, &self.store)
    }

    pub fn validate_tool_setup(&self, input: SetToolSetupInput) -> DetectionReport {
        let mut report = crate::detection::detect_tool_setup(&input.tool_id, &self.store);
        let config = crate::detection::validate_config_dir(
            &input.tool_id,
            &self.store,
            &input.default_config_dir,
        );
        let binary = crate::detection::validate_binary_path(&input.tool_id, &input.binary_path);
        report.config_candidates.insert(0, config);
        report.binary_candidates.insert(0, binary);
        report
    }

    pub fn set_tool_setup(&self, input: SetToolSetupInput) -> Result<AppSnapshot> {
        let (setup, _) = crate::detection::setup_from_manual(
            &input.tool_id,
            &self.store,
            input.binary_path,
            input.default_config_dir,
        );
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.tool_setups
                .insert(input.tool_id.as_str().to_string(), setup);
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Build the token-usage report (Usage tab): incrementally scan every Claude/Codex config
    /// dir on the machine, aggregate per tool, and price it via the LiteLLM cache. Antigravity
    /// is excluded (no token logs). Cheap to call repeatedly thanks to the per-file cursor cache.
    pub fn usage_report(&self, range_days: u32) -> UsageReport {
        crate::usage::build_report(
            &self.store.usage_cache_path(),
            &self.store.price_cache_path(),
            &self.config_dirs(&ToolId::Claude),
            &self.config_dirs(&ToolId::Codex),
            range_days,
        )
    }

    pub fn api_usage_report(&self) -> ApiUsageReport {
        crate::api_gateway::usage_report(&self.store)
    }

    /// Every config dir to scan for a tool: the machine default (`~/.claude`, `~/.codex`) plus
    /// every per-account profile dir under the app's accounts root.
    fn config_dirs(&self, tool_id: &ToolId) -> Vec<std::path::PathBuf> {
        let default_dir = self
            .data
            .lock()
            .ok()
            .map(|data| resolved_default_config_dir(&data, tool_id))
            .unwrap_or_else(|| default_config_dir(tool_id));
        let mut dirs = vec![default_dir];
        if let Ok(entries) = std::fs::read_dir(self.store.tool_accounts_root(tool_id)) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                }
            }
        }
        dirs
    }

    /// Scan EVERY account in `NeedsLogin`: any that already has a token (the user finished
    /// logging in while the app was closed) → move to Idle + read quota. Called on every
    /// snapshot load so the app is correct as soon as it opens, without needing to press Refresh.
    pub fn recheck_pending_logins(&self) -> Result<()> {
        let pending: Vec<(ToolId, String)> = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts
                .iter()
                .filter(|a| a.state == AccountState::NeedsLogin && !a.is_default)
                .map(|a| (a.tool_id.clone(), a.id.clone()))
                .collect()
        };
        for (tool_id, account_id) in pending {
            let _ = self.confirm_login(&tool_id, &account_id);
        }
        Ok(())
    }

    pub fn refresh_tool(&self, tool_id: ToolId, app: Option<&AppHandle>) -> Result<AppSnapshot> {
        // Phase 1: collect (account_id, config_dir) — brief lock, no HTTP.
        let accounts_info: Vec<(String, std::path::PathBuf)> = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let default_config_dir = resolved_default_config_dir(&data, &tool_id);
            data.accounts
                .iter()
                .filter(|a| {
                    a.tool_id == tool_id
                        && a.state != AccountState::NeedsLogin
                        && a.api_provider.is_none()
                })
                .map(|a| {
                    (
                        a.id.clone(),
                        account_config_dir_with_default(&self.store, a, &default_config_dir),
                    )
                })
                .collect()
        };
        // Mutex released — HTTP calls run in parallel without blocking other operations.

        // Phase 2: fetch all quotas in parallel (no mutex held).
        let results: Vec<(String, QuotaInfo)> = {
            let handles: Vec<_> = accounts_info
                .into_iter()
                .map(|(account_id, config_dir)| {
                    let tid = tool_id.clone();
                    std::thread::spawn(move || (account_id, read_quota(&tid, &config_dir)))
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        };

        // Phase 3: write all quotas back — brief lock.
        let exhausted_accounts: Vec<Account> = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let timestamp = now();
            let mut exhausted = Vec::new();
            for (account_id, quota) in results {
                if let Some(account) = data
                    .accounts
                    .iter_mut()
                    .find(|a| a.tool_id == tool_id && a.id == account_id)
                {
                    account.quota = Some(quota);
                    account.updated_at = timestamp.clone();
                    account.state = if is_exhausted(account) {
                        AccountState::Exhausted
                    } else if account.state == AccountState::Exhausted {
                        AccountState::Idle
                    } else {
                        account.state.clone()
                    };
                    if account.state == AccountState::Exhausted {
                        exhausted.push(account.clone());
                    }
                }
            }
            self.store.save(&data)?;
            exhausted
        };

        if let Some(app) = app {
            for account in &exhausted_accounts {
                notify_exhausted(app, account);
            }
        }

        let setting = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            auto_switch_setting(&data, &tool_id)
        };
        if setting.enabled {
            self.maybe_auto_switch(&tool_id, setting.threshold, app)?;
        }
        self.snapshot()
    }

    /// Refresh quota for a single account. Releases the mutex during the HTTP call so other
    /// operations (switch, add) are not blocked while waiting for the network.
    pub fn refresh_single_account(
        &self,
        tool_id: &ToolId,
        account_id: &str,
        app: Option<&AppHandle>,
    ) -> Result<AppSnapshot> {
        // Phase 1: get config_dir — brief lock.
        let config_dir: Option<std::path::PathBuf> = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let default_dir = resolved_default_config_dir(&data, tool_id);
            data.accounts
                .iter()
                .find(|a| {
                    a.tool_id == *tool_id
                        && a.id == account_id
                        && a.state != AccountState::NeedsLogin
                        && a.api_provider.is_none()
                })
                .map(|a| account_config_dir_with_default(&self.store, a, &default_dir))
        };
        let Some(config_dir) = config_dir else {
            return self.snapshot();
        };

        // Phase 2: HTTP call — no mutex held.
        let quota = read_quota(tool_id, &config_dir);

        // Phase 3: write back — brief lock.
        let exhausted: Option<Account> = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let mut result = None;
            if let Some(account) = data
                .accounts
                .iter_mut()
                .find(|a| a.tool_id == *tool_id && a.id == account_id)
            {
                account.quota = Some(quota);
                account.updated_at = now();
                account.state = if is_exhausted(account) {
                    AccountState::Exhausted
                } else if account.state == AccountState::Exhausted {
                    AccountState::Idle
                } else {
                    account.state.clone()
                };
                if account.state == AccountState::Exhausted {
                    result = Some(account.clone());
                }
            }
            self.store.save(&data)?;
            result
        };

        if let (Some(app), Some(account)) = (app, exhausted) {
            notify_exhausted(app, &account);
        }

        let setting = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            auto_switch_setting(&data, tool_id)
        };
        if setting.enabled {
            self.maybe_auto_switch(tool_id, setting.threshold, app)?;
        }
        self.snapshot()
    }

    /// Legacy command: apply the same auto-switch setting to Claude and Codex.
    pub fn set_auto_switch(&self, enabled: bool, threshold: f64) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.auto_switch = enabled;
            data.auto_switch_threshold = threshold.clamp(50.0, 100.0);
            let threshold = data.auto_switch_threshold;
            for tool_id in [ToolId::Claude, ToolId::Codex] {
                data.auto_switch_settings.insert(
                    tool_id.as_str().to_string(),
                    AutoSwitchSetting { enabled, threshold },
                );
            }
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Enable/disable auto-switch + set the threshold for one CLI tool.
    pub fn set_auto_switch_setting(
        &self,
        tool_id: ToolId,
        enabled: bool,
        threshold: f64,
    ) -> Result<AppSnapshot> {
        if matches!(tool_id, ToolId::Antigravity) {
            anyhow::bail!("Auto-switch is only supported for Claude Code and Codex");
        }
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.auto_switch_settings.insert(
                tool_id.as_str().to_string(),
                AutoSwitchSetting {
                    enabled,
                    threshold: threshold.clamp(50.0, 100.0),
                },
            );
            let claude = auto_switch_setting(&data, &ToolId::Claude);
            let codex = auto_switch_setting(&data, &ToolId::Codex);
            data.auto_switch = claude.enabled || codex.enabled;
            data.auto_switch_threshold = if claude.enabled {
                claude.threshold
            } else {
                codex.threshold
            };
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    // ---------------------------------------------------------------------------
    // Auto session prime
    // ---------------------------------------------------------------------------

    /// Set (or clear) the daily prime schedule for one account. Setting overwrites the previous
    /// time — there is exactly one prime time per account, and it persists until changed.
    pub fn set_auto_prime(
        &self,
        tool_id: ToolId,
        account_id: String,
        enabled: bool,
        time: String,
    ) -> Result<AppSnapshot> {
        let time = normalize_hhmm(&time)
            .ok_or_else(|| anyhow::anyhow!("Giờ không hợp lệ (cần dạng HH:MM 24h)"))?;
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            if !prime_eligible_in_state(&data, &tool_id, &account_id) {
                anyhow::bail!("Tài khoản này không hỗ trợ auto prime (chỉ Claude/Codex đăng nhập subscription)");
            }
            let entry = data.auto_prime.entry(account_id).or_default();
            let time_changed = entry.time != time;
            entry.enabled = enabled;
            entry.time = time;
            // Changing the time starts a fresh schedule: clear the "already primed today" guard
            // so the new time can run today even if the old time already primed.
            if time_changed {
                entry.last_primed_date = None;
                entry.last_primed_time = None;
            }
            let _ = tool_id;
            self.store.save(&data)?;
        }
        self.update_wake_schedule(None);
        self.snapshot()
    }

    /// Apply one time + enabled flag to EVERY prime-eligible account ("Apply all").
    pub fn set_auto_prime_all(&self, time: String, enabled: bool) -> Result<AppSnapshot> {
        let time = normalize_hhmm(&time)
            .ok_or_else(|| anyhow::anyhow!("Giờ không hợp lệ (cần dạng HH:MM 24h)"))?;
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let eligible: Vec<String> = data
                .accounts
                .iter()
                .filter(|a| crate::prime::is_prime_eligible(&a.tool_id, a.api_provider.is_some()))
                .map(|a| a.id.clone())
                .collect();
            for account_id in eligible {
                let entry = data.auto_prime.entry(account_id).or_default();
                let time_changed = entry.time != time;
                entry.enabled = enabled;
                entry.time = time.clone();
                if time_changed {
                    entry.last_primed_date = None;
                    entry.last_primed_time = None;
                }
            }
            self.store.save(&data)?;
        }
        self.update_wake_schedule(None);
        self.snapshot()
    }

    /// Read the human-readable auto-prime activity log (newest content at the bottom).
    pub fn auto_prime_log(&self) -> String {
        std::fs::read_to_string(self.store.auto_prime_log_path()).unwrap_or_default()
    }

    /// Recompute the next pmset wake (earliest upcoming prime time, minus the lead) and write it to
    /// the helper's request file. No-op (clears the wake) if the helper isn't installed or nothing
    /// is scheduled. Safe to call after any schedule change. `hold_until` lets a deferred prime ask
    /// for a wake right after the old window ends.
    pub fn update_wake_schedule(&self, hold_until: Option<chrono::DateTime<chrono::Local>>) {
        if !crate::wake::helper_installed() {
            return; // milestone-1 behavior: rely on the app being awake
        }
        let next = self.next_wake_time(hold_until);
        let _ = crate::wake::write_wake_request(&self.store, next);
    }

    /// The earliest moment the Mac should wake: min over enabled accounts of (today/tomorrow's
    /// prime time − lead), combined with any `hold_until` (a deferred prime's retarget). Returns
    /// None if nothing is scheduled.
    fn next_wake_time(
        &self,
        hold_until: Option<chrono::DateTime<chrono::Local>>,
    ) -> Option<chrono::DateTime<chrono::Local>> {
        use chrono::TimeZone;
        let data = self.data.lock().ok()?;
        let now = chrono::Local::now();
        let mut earliest: Option<chrono::DateTime<chrono::Local>> = hold_until;
        for account in &data.accounts {
            let Some(setting) = data.auto_prime.get(&account.id) else {
                continue;
            };
            if !setting.enabled
                || !crate::prime::is_prime_eligible(&account.tool_id, account.api_provider.is_some())
            {
                continue;
            }
            let Some((h, m)) = parse_hhmm(&setting.time) else {
                continue;
            };
            // The next occurrence of HH:MM today, else tomorrow; minus the wake lead.
            let mut target = now
                .date_naive()
                .and_hms_opt(h, m, 0)
                .and_then(|naive| chrono::Local.from_local_datetime(&naive).single())?;
            if target <= now {
                target += chrono::Duration::days(1);
            }
            let wake = target - chrono::Duration::minutes(crate::wake::WAKE_LEAD_MIN);
            earliest = Some(match earliest {
                Some(current) if current <= wake => current,
                _ => wake,
            });
        }
        // Never schedule a wake in the past.
        earliest.filter(|t| *t > now)
    }

    /// On-demand extend (mechanism 2): record the user's answer to the "extend?" prompt.
    /// Accept → prime once the current window ends. Dismiss → clear the request.
    pub fn confirm_extend(
        &self,
        tool_id: ToolId,
        account_id: String,
        accept: bool,
    ) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            if !prime_eligible_in_state(&data, &tool_id, &account_id) {
                anyhow::bail!("Tài khoản này không hỗ trợ auto prime");
            }
            let entry = data.auto_prime.entry(account_id).or_default();
            entry.extend_requested = accept;
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Detect prime-eligible accounts whose 5h window is about to end (≤30 min) and prompt the user
    /// once per window-ending to extend. Called from the background poller. Reminds via a system
    /// notification + flags the account so the UI shows an "extend?" button.
    pub fn check_extend_reminders(&self, app: Option<&AppHandle>) {
        let mut to_remind: Vec<(String, String)> = Vec::new(); // (tool_label, account_name)
        let mut no_response: Vec<String> = Vec::new(); // log lines
        let mut changed = false;
        {
            let mut data = match self.data.lock() {
                Ok(data) => data,
                Err(_) => return,
            };
            // Snapshot (account_id, tool_label, name, reset_at) first to avoid borrow conflicts.
            let candidates: Vec<(String, String, String, String)> = data
                .accounts
                .iter()
                .filter(|a| crate::prime::is_prime_eligible(&a.tool_id, a.api_provider.is_some()))
                .filter_map(|a| {
                    let reset = a.quota.as_ref()?.five_hour.reset_at.clone()?;
                    Some((
                        a.id.clone(),
                        a.tool_id.prime_label().to_string(),
                        a.name.clone(),
                        reset,
                    ))
                })
                .collect();
            for (account_id, tool_label, account_name, reset_at) in candidates {
                let mins = minutes_until(&reset_at);
                let entry = data.auto_prime.entry(account_id).or_default();

                if (0..=EXTEND_THRESHOLD_MIN).contains(&mins) {
                    // Window ending soon: prompt once per this window-ending, unless already accepted.
                    if entry.extend_reminded_reset.as_deref() == Some(reset_at.as_str())
                        || entry.extend_requested
                    {
                        continue;
                    }
                    entry.extend_reminded_reset = Some(reset_at.clone());
                    changed = true;
                    to_remind.push((tool_label, account_name));
                } else if mins < 0 {
                    // Window has ended. If we reminded for THIS window and the user never accepted,
                    // log the "no response" note once, then clear the reminder.
                    if entry.extend_reminded_reset.as_deref() == Some(reset_at.as_str())
                        && !entry.extend_requested
                    {
                        no_response.push(format!(
                            "{tool_label} · account \"{account_name}\" — đã nhắc gia hạn nhưng không có phản hồi, phiên đã hết"
                        ));
                        entry.extend_reminded_reset = None;
                        changed = true;
                    }
                }
            }
            if changed {
                let _ = self.store.save(&data);
            }
        }
        for line in &no_response {
            self.append_prime_log(line);
        }
        if let Some(app) = app {
            for (tool_label, account_name) in &to_remind {
                let _ = app
                    .notification()
                    .builder()
                    .title("Phiên sắp hết")
                    .body(format!(
                        "Phiên {tool_label} account \"{account_name}\" còn {EXTEND_THRESHOLD_MIN} phút. Mở phiên kế tiếp ngay khi hết để code liền mạch không?"
                    ))
                    .show();
            }
            if changed {
                let _ = app.emit("auto-prime-changed", ());
            }
        }
    }

    /// Append one event line to the auto-prime log. Wording is the brainstorm's exact strings.
    fn append_prime_log(&self, line: &str) {
        use std::io::Write;
        let stamped = format!("[{}] {}\n", local_log_timestamp(), line);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.store.auto_prime_log_path())
        {
            let _ = file.write_all(stamped.as_bytes());
        }
    }

    /// The scheduler tick: prime every account whose time has arrived and that hasn't yet primed
    /// for that time today. Runs sequentially with a gap between accounts. Called by the background
    /// thread (every minute) and once at startup (to catch a missed time → "primed muộn"). `late`
    /// marks the startup catch-up so the log says so.
    pub fn run_due_primes(&self, app: Option<&AppHandle>, late: bool) {
        use std::sync::atomic::Ordering;
        // Skip if a previous batch is still running (send retries can take minutes). The flag is
        // cleared at the end of this call; `_guard` ensures that even on an early return.
        if self
            .priming
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        struct ClearOnDrop<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for ClearOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _guard = ClearOnDrop(&self.priming);

        let today = local_today();
        let now_hhmm = local_now_hhmm();

        // Collect due accounts under a brief lock. An account is due when EITHER:
        //   - scheduled: enabled, eligible, its time has been reached, and it hasn't primed for
        //     that exact time today (once/day); OR
        //   - extend: the user accepted "extend?" (mechanism 2) — prime regardless of the time;
        //     prime_account's D2 will HOLD until the running window ends, so the per-minute tick
        //     keeps retrying until it actually opens a new window.
        let due: Vec<PrimeJob> = {
            let data = match self.data.lock() {
                Ok(data) => data,
                Err(_) => return,
            };
            data.accounts
                .iter()
                .filter_map(|account| {
                    let setting = data.auto_prime.get(&account.id)?;
                    if !crate::prime::is_prime_eligible(&account.tool_id, account.api_provider.is_some()) {
                        return None;
                    }
                    let scheduled_due = setting.enabled
                        && now_hhmm.as_str() >= setting.time.as_str()
                        && !(setting.last_primed_date.as_deref() == Some(today.as_str())
                            && setting.last_primed_time.as_deref() == Some(setting.time.as_str()));
                    let extend_due = setting.extend_requested;
                    if !scheduled_due && !extend_due {
                        return None;
                    }
                    let default_dir = resolved_default_config_dir(&data, &account.tool_id);
                    let config_dir =
                        account_config_dir_with_default(&self.store, account, &default_dir);
                    let claude_binary = configured_binary_path(&data, &account.tool_id);
                    Some(PrimeJob {
                        tool_id: account.tool_id.clone(),
                        account_id: account.id.clone(),
                        account_name: account.name.clone(),
                        config_dir,
                        claude_binary,
                        is_extend: extend_due && !scheduled_due,
                    })
                })
                .collect()
        };

        for (index, job) in due.iter().enumerate() {
            if index > 0 {
                std::thread::sleep(std::time::Duration::from_secs(10)); // gap between accounts
            }
            let outcome = crate::prime::prime_account(
                &job.tool_id,
                &job.config_dir,
                job.claude_binary.as_deref(),
                std::thread::sleep,
            );
            self.record_prime_outcome(job, &outcome, late, app);
        }
    }

    /// Persist the per-account result + append the matching log line, and refresh the account's
    /// displayed quota on success so the new reset shows immediately.
    fn record_prime_outcome(
        &self,
        job: &PrimeJob,
        outcome: &crate::prime::PrimeOutcome,
        late: bool,
        app: Option<&AppHandle>,
    ) {
        use crate::prime::PrimeOutcome;
        let tool_id = &job.tool_id;
        let account_id = job.account_id.as_str();
        let account_name = job.account_name.as_str();
        let is_extend = job.is_extend;
        let tool_label = tool_id.prime_label();
        let (result, line): (&str, String) = match outcome {
            PrimeOutcome::Success { new_reset_at } => {
                let reset = local_hhmm_from_iso(new_reset_at);
                if late {
                    (
                        "success",
                        format!(
                            "{tool_label} · account \"{account_name}\" — SUCCESS: primed muộn lúc {now} do máy không thức đúng giờ, reset mới = {reset}",
                            now = local_now_hhmm()
                        ),
                    )
                } else if is_extend {
                    (
                        "success",
                        format!("{tool_label} · account \"{account_name}\" — SUCCESS: gia hạn, phiên mới bắt đầu, reset mới = {reset}"),
                    )
                } else {
                    (
                        "success",
                        format!("{tool_label} · account \"{account_name}\" — SUCCESS: primed, reset mới = {reset}"),
                    )
                }
            }
            PrimeOutcome::Hold { reset_at } => (
                "hold",
                format!(
                    "{tool_label} · account \"{account_name}\" — HOÃN: phiên cũ còn tới {reset}, sẽ thử lại khi phiên cũ hết",
                    reset = local_hhmm_from_iso(reset_at)
                ),
            ),
            PrimeOutcome::SkipNoToken => (
                "skip",
                format!("{tool_label} · account \"{account_name}\" — SKIP: token hết hạn, cần đăng nhập lại"),
            ),
            PrimeOutcome::FailSend { reason } => (
                "failed",
                format!("{tool_label} · account \"{account_name}\" — FAIL: gửi lỗi sau 5 lần thử ({reason})"),
            ),
            PrimeOutcome::FailUnconfirmed => (
                "failed",
                format!("{tool_label} · account \"{account_name}\" — FAIL: gửi OK nhưng không xác nhận được phiên mới sau 5 lần kiểm tra"),
            ),
        };
        self.append_prime_log(&line);

        // Update the per-account schedule record. Mark "primed today for this time" only when we
        // actually opened (or attempted to open) a new window — HOLD/SKIP don't consume the day.
        if let Ok(mut data) = self.data.lock() {
            if let Some(setting) = data.auto_prime.get_mut(account_id) {
                setting.last_result = Some(result.to_string());
                setting.last_attempt_at = Some(now());
                if matches!(
                    outcome,
                    PrimeOutcome::Success { .. } | PrimeOutcome::FailUnconfirmed
                ) {
                    setting.last_primed_date = Some(local_today());
                    setting.last_primed_time = Some(setting.time.clone());
                    // The extend request is fulfilled once a new window opens (or we gave up
                    // confirming it) — clear it so it doesn't re-fire on the next tick.
                    setting.extend_requested = false;
                }
            }
            let _ = self.store.save(&data);
        }

        match outcome {
            // On success, refresh the displayed quota right away + recompute the next wake.
            PrimeOutcome::Success { .. } => {
                let _ = self.refresh_single_account(tool_id, account_id, app);
                self.update_wake_schedule(None);
                if let Some(app) = app {
                    let _ = app.emit("auto-prime-changed", ());
                }
            }
            // Held because the old window is still active → ask the Mac to wake again right after
            // it ends (reset_at + 5'), so a deferred prime fires promptly even from sleep.
            PrimeOutcome::Hold { reset_at } => {
                let retarget = chrono::DateTime::parse_from_rfc3339(reset_at)
                    .ok()
                    .map(|t| t.with_timezone(&chrono::Local) + chrono::Duration::minutes(5));
                self.update_wake_schedule(retarget);
            }
            _ => {}
        }
    }

    /// If the tool's currently-used account (plain command) has hit the threshold → automatically
    /// switch to the healthiest account (same tool, not yet at the threshold). Claude/Codex only.
    /// Applied via the same switch mechanism (hook + active file), WITHOUT touching custom launchers.
    fn maybe_auto_switch(
        &self,
        tool_id: &ToolId,
        threshold: f64,
        app: Option<&AppHandle>,
    ) -> Result<()> {
        if !matches!(tool_id, ToolId::Claude | ToolId::Codex) {
            return Ok(());
        }
        let target_id = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let active = data
                .accounts
                .iter()
                .find(|a| a.tool_id == *tool_id && a.state == AccountState::Active);
            // No active account (using Default) or the active one still has quota → no switch needed.
            match active {
                Some(active) if max_percent_used(active) >= threshold => {
                    best_replacement(&data.accounts, tool_id, threshold, Some(&active.id))
                        .map(|a| a.id.clone())
                }
                _ => None,
            }
        };

        let Some(target_id) = target_id else {
            return Ok(());
        };

        write_active_profile(tool_id, &self.store, &target_id)
            .context("Auto-switch failed while writing the active profile")?;
        install_shell_hook(&self.store).context("Auto-switch failed while installing the hook")?;

        let switched_name = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let timestamp = now();
            normalize_account_states(&mut data.accounts, tool_id, Some(&target_id));
            let mut name = String::new();
            for account in data.accounts.iter_mut().filter(|a| a.tool_id == *tool_id) {
                if account.id == target_id {
                    account.state = AccountState::Active;
                    account.last_used_at = Some(timestamp.clone());
                    account.updated_at = timestamp.clone();
                    name = account.name.clone();
                }
            }
            self.store.save(&data)?;
            name
        };

        if let Some(app) = app {
            let message = format!(
                "{} is out of quota — auto-switched to {}. Open a new terminal to apply.",
                tool_id.display_name(),
                switched_name
            );
            let _ = app
                .notification()
                .builder()
                .title("Auto-switched account")
                .body(&message)
                .show();
            // In-app banner (more reliable than the system notification if the user disabled the permission).
            let _ = app.emit("auto-switched", message);
            if let Ok(snapshot) = self.snapshot() {
                let _ = app.emit("snapshot-changed", snapshot);
            }
        }
        Ok(())
    }

    /// Called by the background poll: if the account has finished logging in (the token exists) →
    /// move NeedsLogin to Idle + read the real quota. Returns true once the token is present.
    pub fn confirm_login(&self, tool_id: &ToolId, account_id: &str) -> Result<bool> {
        let config_dir = self.store.account_dir(tool_id, account_id);
        if !crate::tools::profile_has_credentials(tool_id, &config_dir) {
            return Ok(false);
        }
        // Seed the onboarding flag only after login completes (claude auth login overwrites
        // .claude.json, so it must be seeded AFTERWARD) — so interactive mode skips the wizard.
        // Re-link the shared session after login too (login may create the real dir).
        crate::tools::seed_onboarding(tool_id, &config_dir);
        if let Some(default_dir) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            configured_default_config_dir(&data, tool_id)
        } {
            link_shared_sessions_to(tool_id, &config_dir, &default_dir);
            link_shared_config_to(tool_id, &config_dir, &default_dir);
        }
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        if let Some(account) = data
            .accounts
            .iter_mut()
            .find(|account| account.tool_id == *tool_id && account.id == account_id)
        {
            if account.state == AccountState::NeedsLogin {
                account.state = AccountState::Idle;
                account.quota = Some(read_quota(tool_id, &config_dir));
                account.updated_at = now();
            }
        }
        self.store.save(&data)?;
        Ok(true)
    }

    pub fn add_account(&self, app: &AppHandle, input: AddAccountInput) -> Result<AppSnapshot> {
        validate_name(&input.tool_id, None, &input.name, self)?;
        // Antigravity IDE: each account = its own userData; open the IDE with --user-data-dir
        // to log in a new Google account (the login lives in that dir's state.vscdb).
        if matches!(input.tool_id, ToolId::Antigravity) {
            return self.create_antigravity_account(input);
        }
        // Claude/Codex: create the profile + custom command, then open Terminal to log in.
        self.create_profile_account(app, input)
    }

    /// Save the Antigravity IDE account currently logged in: capture the token from the default
    /// state.vscdb. The user must ensure the IDE is logged into the exact account they want to save.
    fn create_antigravity_account(&self, input: AddAccountInput) -> Result<AppSnapshot> {
        // The IDE only writes the token to state.vscdb on EXIT (lazy write) → quit to flush
        // the logged-in session, read the token, then reopen the IDE for the user (regardless of capture outcome).
        antigravity_quit_ide();
        let id = Uuid::new_v4().to_string();
        let captured = antigravity_capture(&self.store, &id);
        let _ = antigravity_open_ide();
        let fingerprint = captured.context("Failed to save account")?;

        // Don't save duplicates: the same Google account = the same avatar (even if the token blob
        // differs across two captures). Fall back to comparing tokens if the avatar is missing.
        // Remove the just-captured dir and report.
        let new_profile = antigravity_saved_profile(&self.store, &id);
        let new_token = antigravity_saved_token(&self.store, &id);
        {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let dup = data.accounts.iter().any(|account| {
                if account.tool_id != ToolId::Antigravity {
                    return false;
                }
                match (
                    &new_profile,
                    antigravity_saved_profile(&self.store, &account.id),
                ) {
                    (Some(new), Some(existing)) => *new == existing,
                    _ => antigravity_saved_token(&self.store, &account.id) == new_token,
                }
            });
            if dup {
                drop(data);
                let _ = delete_account_files(&ToolId::Antigravity, &self.store, &id);
                anyhow::bail!("This account is already saved");
            }
        }

        let name = normalized_or_default_name(&ToolId::Antigravity, &input.name, self)?;
        let quota = read_quota(
            &ToolId::Antigravity,
            &default_config_dir(&ToolId::Antigravity),
        );
        let timestamp = now();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts.push(Account {
                id,
                tool_id: ToolId::Antigravity,
                name,
                state: AccountState::Idle,
                fingerprint,
                created_at: timestamp.clone(),
                updated_at: timestamp,
                last_used_at: None,
                quota: Some(quota),
                launcher_command: None,
                is_default: false,
                avatar_url: None,
                api_provider: None,
            });
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    fn create_profile_account(
        &self,
        app: &AppHandle,
        input: AddAccountInput,
    ) -> Result<AppSnapshot> {
        // A launcher is required for Claude/Codex — it's the ONLY way to use the account.
        let raw_launcher = input
            .launcher
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .context("A custom command is required for the account (e.g. claude-work)")?;
        let id = Uuid::new_v4().to_string();
        let full_launcher = self.validated_launcher(&input.tool_id, &id, raw_launcher)?;
        let name = normalized_or_default_name(&input.tool_id, &input.name, self)?;

        let default_dir = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            configured_default_config_dir(&data, &input.tool_id)
        }
        .context("CLI setup is ambiguous — choose the tool's binary and default config first")?;
        let binary_path = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            configured_binary_path(&data, &input.tool_id)
        }
        .context("CLI setup is ambiguous — choose the tool's binary first")?;
        launch_profile_login(&input.tool_id, &self.store, &id, &default_dir, &binary_path)
            .context("Login not completed, account not added")?;
        write_launcher(
            &input.tool_id,
            &self.store,
            &id,
            &full_launcher,
            &binary_path,
        )
        .context("Couldn't create the account's custom command")?;

        let timestamp = now();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts.push(Account {
                id: id.clone(),
                tool_id: input.tool_id.clone(),
                name,
                state: AccountState::NeedsLogin,
                fingerprint: format!("profile:{id}"),
                created_at: timestamp.clone(),
                updated_at: timestamp,
                last_used_at: None,
                quota: Some(crate::models::QuotaInfo::with_message(
                    "Waiting for login in Terminal — the app will detect it when done",
                )),
                launcher_command: Some(full_launcher),
                is_default: false,
                avatar_url: None,
                api_provider: None,
            });
            self.store.save(&data)?;
        }

        // Background poll: login done (token appears) → NeedsLogin to Idle + read quota.
        spawn_login_watch(app.clone(), input.tool_id.clone(), id);
        self.snapshot()
    }

    /// Add an API/proxy account (Codex): create the profile, write the gateway config + key, and an
    /// optional custom command. No OAuth login → the account is ready (Idle) immediately, with no quota.
    pub fn add_api_account(&self, input: AddApiAccountInput) -> Result<AppSnapshot> {
        if !matches!(input.tool_id, ToolId::Codex | ToolId::Claude) {
            anyhow::bail!("API/proxy accounts are only supported for Codex and Claude Code");
        }
        validate_name(&input.tool_id, None, &input.name, self)?;

        let base_url = input.base_url.trim().to_string();
        if !base_url.starts_with("https://") {
            anyhow::bail!("Gateway URL must start with https://");
        }
        let api_key = input.api_key.trim().to_string();
        if api_key.is_empty() {
            anyhow::bail!("API key is required");
        }
        let model = input.model.trim().to_string();
        if model.is_empty() {
            anyhow::bail!("Pick a model");
        }

        let id = Uuid::new_v4().to_string();
        let name = normalized_or_default_name(&input.tool_id, &input.name, self)?;

        // Validate the optional launcher up front (collision/charset) before writing anything.
        let full_launcher = match input
            .launcher
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(raw) => Some(self.validated_launcher(&input.tool_id, &id, raw)?),
            None => None,
        };

        let default_dir = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            configured_default_config_dir(&data, &input.tool_id)
        }
        .context("CLI setup is ambiguous — choose the tool's binary and default config first")?;
        let profile = create_profile_with_default(&input.tool_id, &self.store, &id, &default_dir)?;
        let binary_path = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            configured_binary_path(&data, &input.tool_id)
        }
        .context("CLI setup is ambiguous — choose the tool's binary first")?;
        match input.tool_id {
            ToolId::Codex => {
                write_codex_proxy_config(&profile, &name, &base_url, &model)
                    .context("Couldn't write the Codex config")?;
                write_api_key_file(&profile, &api_key).context("Couldn't store the API key")?;
            }
            ToolId::Claude => {
                write_claude_proxy_settings(&profile, &base_url, &api_key, &model)
                    .context("Couldn't write the Claude settings")?;
                // Skip the first-run wizard so the bare command / launcher start straight into a session.
                crate::tools::seed_onboarding(&input.tool_id, &profile);
            }
            ToolId::Antigravity => unreachable!("guarded above"),
        }
        if let Some(full) = &full_launcher {
            write_api_launcher(
                &input.tool_id,
                &self.store,
                &id,
                full,
                &model,
                input.bypass,
                &binary_path,
            )
            .context("Couldn't create the account's custom command")?;
        }

        let timestamp = now();
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts.push(Account {
                id,
                tool_id: input.tool_id.clone(),
                name,
                state: AccountState::Idle,
                fingerprint: "api".to_string(),
                created_at: timestamp.clone(),
                updated_at: timestamp,
                last_used_at: None,
                // API/proxy gateways expose no quota — hide the bars.
                quota: None,
                launcher_command: full_launcher,
                is_default: false,
                avatar_url: None,
                api_provider: Some(ApiProvider {
                    base_url,
                    model,
                    bypass: input.bypass,
                }),
            });
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    pub fn rename_account(&self, input: RenameAccountInput) -> Result<AppSnapshot> {
        validate_name(&input.tool_id, Some(&input.account_id), &input.name, self)?;
        let name = if input.name.trim().is_empty() {
            normalized_or_default_name(&input.tool_id, "", self)?
        } else {
            input.name.trim().to_string()
        };

        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter_mut()
                .find(|account| account.tool_id == input.tool_id && account.id == input.account_id)
                .context("Account not found")?;
            if account.is_default {
                anyhow::bail!("Can't rename the machine default account");
            }
            if is_virtual_api_account(account) {
                anyhow::bail!("Local API accounts are managed from the API tab");
            }
            account.name = name;
            account.updated_at = now();
            self.store.save(&data)?;
        }

        self.snapshot()
    }

    /// Set/rename the account's custom command (Claude/Codex).
    pub fn set_launcher(&self, input: SetLauncherInput) -> Result<AppSnapshot> {
        let raw = input.name.trim();
        if raw.is_empty() {
            anyhow::bail!("Command name is empty");
        }
        let full = self.validated_launcher(&input.tool_id, &input.account_id, raw)?;
        let (old_launcher, api) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|a| a.tool_id == input.tool_id && a.id == input.account_id)
                .context("Account not found")?;
            if is_virtual_api_account(account) {
                anyhow::bail!("Local API accounts are managed from the API tab");
            }
            (
                account.launcher_command.clone(),
                account
                    .api_provider
                    .as_ref()
                    .map(|p| (p.model.clone(), p.bypass)),
            )
        };

        // API/proxy accounts need a launcher that exports the key + pins the model (+ optional bypass).
        match api {
            Some((model, bypass)) => {
                let binary_path = {
                    let data = self
                        .data
                        .lock()
                        .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
                    configured_binary_path(&data, &input.tool_id)
                }
                .context("CLI setup is ambiguous — choose the tool's binary first")?;
                write_api_launcher(
                    &input.tool_id,
                    &self.store,
                    &input.account_id,
                    &full,
                    &model,
                    bypass,
                    &binary_path,
                )
                .context("Couldn't create the custom command")?;
            }
            None => {
                let binary_path = {
                    let data = self
                        .data
                        .lock()
                        .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
                    configured_binary_path(&data, &input.tool_id)
                }
                .context("CLI setup is ambiguous — choose the tool's binary first")?;
                write_launcher(
                    &input.tool_id,
                    &self.store,
                    &input.account_id,
                    &full,
                    &binary_path,
                )
                .context("Couldn't create the custom command")?;
            }
        }
        if let Some(old) = old_launcher.filter(|old| old != &full) {
            remove_launcher(&old);
        }

        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            if let Some(account) = data
                .accounts
                .iter_mut()
                .find(|a| a.tool_id == input.tool_id && a.id == input.account_id)
            {
                account.launcher_command = Some(full);
                account.updated_at = now();
            }
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Switch = pick the account for the PLAIN `claude`/`codex` command (via shell hook +
    /// active file, WITHOUT wrapping the binary). Antigravity still copy-swaps credentials.
    pub fn switch_account(&self, input: SwitchAccountInput) -> Result<AppSnapshot> {
        let (is_default, state) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|account| account.tool_id == input.tool_id && account.id == input.account_id)
                .context("Failed to switch account — kept the previous account")?;
            (account.is_default, account.state.clone())
        };

        if state == AccountState::NeedsLogin {
            anyhow::bail!("Account hasn't finished logging in yet");
        }

        match input.tool_id {
            ToolId::Claude | ToolId::Codex => {
                if is_default {
                    clear_active_profile(&input.tool_id, &self.store)
                        .context("Failed to switch account — kept the previous account")?;
                } else {
                    write_active_profile(&input.tool_id, &self.store, &input.account_id)
                        .context("Failed to switch account — kept the previous account")?;
                }
                install_shell_hook(&self.store)
                    .context("Couldn't install the shell hook (~/.zshrc)")?;
            }
            ToolId::Antigravity => {
                // Quit the IDE (to avoid overwriting state.vscdb on exit) → write the chosen
                // account's token into the default state.vscdb → reopen the IDE logged into that account.
                antigravity_quit_ide();
                antigravity_restore(&self.store, &input.account_id)
                    .context("Failed to switch account — check Antigravity IDE")?;
                let _ = antigravity_open_ide();
            }
        }

        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let timestamp = now();
            normalize_account_states(&mut data.accounts, &input.tool_id, Some(&input.account_id));
            for account in data.accounts.iter_mut().filter(|account| {
                account.tool_id == input.tool_id && account.id == input.account_id
            }) {
                account.state = if is_exhausted(account) {
                    AccountState::Exhausted
                } else {
                    AccountState::Active
                };
                account.last_used_at = Some(timestamp.clone());
                account.updated_at = timestamp.clone();
            }
            self.store.save(&data)?;
        }

        self.snapshot()
    }

    pub fn delete_account(&self, tool_id: ToolId, account_id: String) -> Result<AppSnapshot> {
        let (launcher, was_active) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|a| a.tool_id == tool_id && a.id == account_id)
                .context("Account not found")?;
            if account.is_default {
                anyhow::bail!("Can't delete the machine default account");
            }
            (
                account.launcher_command.clone(),
                account.state == AccountState::Active,
            )
        };

        delete_account_files(&tool_id, &self.store, &account_id)?;
        if let Some(name) = launcher {
            remove_launcher(&name);
        }
        // Deleting the active account (the one the plain command uses) → clear the active file so
        // the plain command falls back to the machine Default, NOT leaving it pointing to a deleted profile.
        if was_active && matches!(tool_id, ToolId::Claude | ToolId::Codex) {
            let _ = clear_active_profile(&tool_id, &self.store);
            let _ = install_shell_hook(&self.store);
        }

        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts
                .retain(|account| !(account.tool_id == tool_id && account.id == account_id));
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    pub fn accept_disclaimer(&self) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.disclaimer_accepted = true;
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    /// Bring the Antigravity IDE to the login screen to add an account that has never logged in.
    pub fn antigravity_new_login(&self) -> Result<AppSnapshot> {
        let saved: Vec<String> = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts
                .iter()
                .filter(|account| account.tool_id == ToolId::Antigravity)
                .filter_map(|account| antigravity_saved_token(&self.store, &account.id))
                .collect()
        };
        antigravity_new_login(&saved)?;
        self.snapshot()
    }

    /// Normalize + validate the command name: enforce the prefix and charset, no collision with
    /// another account's launcher, and no overriding a system binary.
    fn validated_launcher(&self, tool_id: &ToolId, account_id: &str, raw: &str) -> Result<String> {
        let full = full_launcher_name(tool_id, raw)?;
        {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let dup = data.accounts.iter().any(|a| {
                a.id != account_id && a.launcher_command.as_deref() == Some(full.as_str())
            });
            if dup {
                anyhow::bail!("Command '{full}' is already used by another account");
            }
        }
        if launcher_name_collides_with_system(&full) {
            anyhow::bail!("Command '{full}' conflicts with an existing system command");
        }
        Ok(full)
    }
}

/// Watch in the background after opening Terminal to log in: check every ~2s whether the token
/// exists, for up to ~3 minutes. Token present → confirm_login + emit to update the UI.
fn spawn_login_watch(app: AppHandle, tool_id: ToolId, account_id: String) {
    std::thread::spawn(move || {
        for _ in 0..90 {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let state = app.state::<ManagedState>();
            if let Ok(true) = state.confirm_login(&tool_id, &account_id) {
                if let Ok(snapshot) = state.snapshot() {
                    let _ = app.emit("snapshot-changed", snapshot);
                }
                break;
            }
        }
    });
}

/// The account's config dir for reading quota: profile accounts read from their own directory,
/// the rest (default, antigravity import) read from the machine's default config dir.
fn account_config_dir_with_default(
    store: &Store,
    account: &Account,
    default_config_dir: &std::path::Path,
) -> std::path::PathBuf {
    if account.fingerprint.starts_with("profile:") {
        store.account_dir(&account.tool_id, &account.id)
    } else {
        default_config_dir.to_path_buf()
    }
}

fn resolved_default_config_dir(data: &StoredState, tool_id: &ToolId) -> std::path::PathBuf {
    configured_default_config_dir(data, tool_id).unwrap_or_else(|| default_config_dir(tool_id))
}

fn configured_default_config_dir(
    data: &StoredState,
    tool_id: &ToolId,
) -> Option<std::path::PathBuf> {
    data.tool_setups
        .get(tool_id.as_str())
        .and_then(|setup| setup.default_config_dir.clone())
}

fn configured_binary_path(data: &StoredState, tool_id: &ToolId) -> Option<std::path::PathBuf> {
    data.tool_setups
        .get(tool_id.as_str())
        .and_then(|setup| setup.binary_path.clone())
}

fn default_auto_switch_setting_from_legacy(data: &StoredState) -> AutoSwitchSetting {
    AutoSwitchSetting {
        enabled: data.auto_switch,
        threshold: data.auto_switch_threshold.clamp(50.0, 100.0),
    }
}

fn auto_switch_setting(data: &StoredState, tool_id: &ToolId) -> AutoSwitchSetting {
    data.auto_switch_settings
        .get(tool_id.as_str())
        .cloned()
        .unwrap_or_else(|| default_auto_switch_setting_from_legacy(data))
}

/// Drop the old-style "system-default" account, ensuring each CLI tool has one machine Default
/// account (pointing to ~/.claude, ~/.codex) — read-only, reading quota like a normal account.
fn migrate_defaults(accounts: &mut Vec<Account>) {
    accounts.retain(|a| a.id != "system-default");
    // Antigravity (capture/swap) has no machine Default account — remove the old one if present.
    accounts.retain(|a| a.id != "default-antigravity");
    for tool_id in [ToolId::Claude, ToolId::Codex] {
        let default_id = format!("default-{}", tool_id.as_str());
        if accounts.iter().any(|a| a.id == default_id) {
            continue;
        }
        let timestamp = now();
        accounts.push(Account {
            id: default_id,
            tool_id: tool_id.clone(),
            name: "Machine default".to_string(),
            state: AccountState::Idle,
            fingerprint: "default".to_string(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            last_used_at: None,
            quota: Some(crate::models::QuotaInfo::with_message(
                "Click Refresh to read quota",
            )),
            launcher_command: None,
            is_default: true,
            avatar_url: None,
            api_provider: None,
        });
    }
}

fn migrate_auto_switch_settings(data: &mut StoredState) {
    let legacy = default_auto_switch_setting_from_legacy(data);
    for tool_id in [ToolId::Claude, ToolId::Codex] {
        data.auto_switch_settings
            .entry(tool_id.as_str().to_string())
            .or_insert_with(|| legacy.clone());
    }
    data.auto_switch_settings
        .remove(ToolId::Antigravity.as_str());
    data.auto_switch = data
        .auto_switch_settings
        .values()
        .any(|setting| setting.enabled);
}

fn build_snapshot(
    store: &Store,
    data: &StoredState,
    status: crate::models::ApiGatewayStatus,
) -> AppSnapshot {
    let show_virtual_api = status.state == ApiGatewayServerState::Running;
    let tools = [ToolId::Claude, ToolId::Codex, ToolId::Antigravity]
        .into_iter()
        .map(|tool_id| {
            let mut accounts = data
                .accounts
                .iter()
                .filter(|account| {
                    account.tool_id == tool_id
                        && (show_virtual_api || !is_virtual_api_account(account))
                })
                .cloned()
                .collect::<Vec<_>>();
            // Default first, the rest by name.
            accounts.sort_by(|a, b| {
                b.is_default
                    .cmp(&a.is_default)
                    .then_with(|| a.name.cmp(&b.name))
            });
            // Antigravity: attach the Google avatar (account identity) for the UI to display.
            if matches!(tool_id, ToolId::Antigravity) {
                for account in accounts.iter_mut() {
                    account.avatar_url = crate::tools::antigravity_avatar_url(store, &account.id);
                }
            }
            let active_account_id = active_account_id_for(store, &tool_id, &accounts);
            ToolStatus {
                id: tool_id.clone(),
                name: tool_id.display_name().to_string(),
                installed: is_installed_resolved(data, &tool_id),
                active_account_id,
                accounts,
            }
        })
        .collect();

    AppSnapshot {
        tools,
        disclaimer_accepted: data.disclaimer_accepted,
        auto_switch: data.auto_switch,
        auto_switch_threshold: data.auto_switch_threshold,
        auto_switch_settings: data.auto_switch_settings.clone(),
        auto_prime: data.auto_prime.clone(),
        tool_setups: data.tool_setups.clone(),
        api_gateway: ApiGatewaySnapshot {
            config: redacted_api_gateway_config(data),
            status,
        },
    }
}

fn redacted_api_gateway_config(data: &StoredState) -> ApiGatewayConfig {
    use crate::models::{ApiGatewayAccount, ApiPoolAccountState};
    let mut redacted = data.api_gateway.clone();
    for key in &mut redacted.keys {
        key.secret = None;
    }
    // Surface every eligible subscription account with a live participation state. Accounts with
    // no stored entry default to enabled; the UI renders this list for on/off toggles + status.
    let mut accounts = Vec::new();
    for account in data
        .accounts
        .iter()
        .filter(|account| matches!(account.tool_id, ToolId::Claude | ToolId::Codex))
        .filter(|account| account.api_provider.is_none())
    {
        let stored = data
            .api_gateway
            .accounts
            .iter()
            .find(|entry| entry.tool_id == account.tool_id && entry.account_id == account.id);
        let enabled = stored.is_none_or(|entry| entry.enabled);
        let mut state = ApiPoolAccountState::Available;
        let mut cooldown_until = None;
        let mut error = None;
        if !enabled {
            state = ApiPoolAccountState::Excluded;
        } else if matches!(account.state, AccountState::NeedsLogin) {
            state = ApiPoolAccountState::Errored;
            error = Some("Account needs login".to_string());
        } else if max_percent_used(account) >= data.api_gateway.quota_threshold {
            state = ApiPoolAccountState::Exhausted;
        } else {
            let cooling = stored
                .and_then(|entry| entry.cooldown_until.as_deref())
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|until| until > chrono::Utc::now());
            if cooling {
                state = ApiPoolAccountState::CoolingDown;
                cooldown_until = stored.and_then(|entry| entry.cooldown_until.clone());
            } else if matches!(
                stored.map(|entry| &entry.state),
                Some(ApiPoolAccountState::Errored)
            ) {
                state = ApiPoolAccountState::Errored;
                error = stored.and_then(|entry| entry.error.clone());
            }
        }
        accounts.push(ApiGatewayAccount {
            tool_id: account.tool_id.clone(),
            account_id: account.id.clone(),
            enabled,
            state,
            cooldown_until,
            error,
        });
    }
    redacted.accounts = accounts;
    redacted
}

fn autodetect_missing_tool_setups(store: &Store, data: &mut StoredState) {
    for tool_id in [ToolId::Claude, ToolId::Codex] {
        if data.tool_setups.contains_key(tool_id.as_str()) {
            continue;
        }
        let report = crate::detection::detect_tool_setup(&tool_id, store);
        if let Some(setup) = report.resolution.setup {
            data.tool_setups.insert(tool_id.as_str().to_string(), setup);
        }
    }
}

fn is_installed_resolved(data: &StoredState, tool_id: &ToolId) -> bool {
    data.tool_setups
        .get(tool_id.as_str())
        .and_then(|setup| setup.binary_path.as_ref())
        .is_some_and(|path| path.exists())
        || is_installed(tool_id)
}

/// The account the PLAIN COMMAND is using. For Claude/Codex this is the real source of truth:
/// the active file (`active/<tool>.profile`) that the shell hook reads to export the config dir —
/// NOT inferred from `state==Active` (an exhausted account is still the one the plain command uses,
/// but its state is Exhausted, so inferring from state would be wrong). Empty/missing file = machine Default.
/// Antigravity is copy-swap (no active file), so it still follows `state==Active`.
fn active_account_id_for(store: &Store, tool_id: &ToolId, accounts: &[Account]) -> Option<String> {
    if matches!(tool_id, ToolId::Antigravity) {
        // The account in use = the account whose token matches the IDE's current token in state.vscdb.
        let current = antigravity_current_token()?;
        return accounts
            .iter()
            .find(|account| {
                antigravity_saved_token(store, &account.id).as_deref() == Some(current.as_str())
            })
            .map(|account| account.id.clone());
    }

    let target = std::fs::read_to_string(store.active_profile_path(tool_id))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());

    match target {
        // The active file points to a specific account's profile dir → that account is in use.
        Some(target) => accounts
            .iter()
            .find(|account| {
                !account.is_default
                    && store.account_dir(tool_id, &account.id).to_string_lossy() == target
            })
            .map(|account| account.id.clone()),
        // No active file → the plain command uses the machine Default.
        None => accounts
            .iter()
            .find(|account| account.is_default)
            .map(|account| account.id.clone()),
    }
}

/// The account's highest % used (5h or weekly); 0 if there's no data.
fn max_percent_used(account: &Account) -> f64 {
    account.quota.as_ref().map_or(0.0, |quota| {
        if quota.error.is_some() {
            return 0.0;
        }
        [quota.five_hour.percent_used, quota.weekly.percent_used]
            .into_iter()
            .flatten()
            .fold(0.0_f64, f64::max)
    })
}

/// The best replacement account: same tool, not `excluded`, not yet at the threshold,
/// logged in (Idle/Active), with the most quota left. The Default account is eligible.
fn best_replacement<'a>(
    accounts: &'a [Account],
    tool_id: &ToolId,
    threshold: f64,
    excluded_account_id: Option<&str>,
) -> Option<&'a Account> {
    accounts
        .iter()
        .filter(|account| {
            &account.tool_id == tool_id
                && Some(account.id.as_str()) != excluded_account_id
                && !matches!(account.state, AccountState::NeedsLogin)
                && max_percent_used(account) < threshold
                // Skip accounts with no quota data yet (unsure whether any is left).
                && account.quota.as_ref().is_some_and(|q| q.error.is_none())
        })
        .min_by(|left, right| max_percent_used(left).total_cmp(&max_percent_used(right)))
}

fn validate_name(
    tool_id: &ToolId,
    account_id: Option<&str>,
    name: &str,
    state: &ManagedState,
) -> Result<()> {
    let trimmed = name.trim();
    if trimmed.chars().count() > 20 {
        anyhow::bail!("Account name is limited to 20 characters");
    }
    if trimmed.is_empty() {
        return Ok(());
    }
    if [
        virtual_api_name(&ToolId::Claude),
        virtual_api_name(&ToolId::Codex),
    ]
    .contains(&trimmed)
    {
        anyhow::bail!("This account name is reserved for the local API gateway");
    }
    let data = state
        .data
        .lock()
        .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
    let duplicate = data.accounts.iter().any(|account| {
        &account.tool_id == tool_id
            && account.name == trimmed
            && Some(account.id.as_str()) != account_id
    });
    if duplicate {
        anyhow::bail!("Account name must be unique within the same tool");
    }
    Ok(())
}

fn virtual_api_name(tool_id: &ToolId) -> &'static str {
    match tool_id {
        ToolId::Claude => "claude-api",
        ToolId::Codex => "codex-api",
        ToolId::Antigravity => "antigravity-api",
    }
}

fn is_virtual_api_account(account: &Account) -> bool {
    account.fingerprint == "api-local"
}

fn generate_api_key() -> String {
    use rand::RngCore;
    let mut bytes = [0_u8; 24];
    rand::thread_rng().fill_bytes(&mut bytes);
    let encoded = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sk-{encoded}")
}

fn mask_key(secret: &str) -> String {
    let suffix = secret
        .chars()
        .rev()
        .take(6)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("sk-...{suffix}")
}

fn normalized_or_default_name(
    tool_id: &ToolId,
    name: &str,
    state: &ManagedState,
) -> Result<String> {
    let trimmed = name.trim();
    if !trimmed.is_empty() {
        return Ok(trimmed.to_string());
    }

    let data = state
        .data
        .lock()
        .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
    let base = tool_id.display_name().replace(" Code", "");
    for index in 1.. {
        let candidate = format!("{base} {index}");
        let exists = data
            .accounts
            .iter()
            .any(|account| &account.tool_id == tool_id && account.name == candidate);
        if !exists {
            return Ok(candidate.chars().take(20).collect());
        }
    }
    unreachable!()
}

fn is_exhausted(account: &Account) -> bool {
    account.quota.as_ref().is_some_and(|quota| {
        quota.error.is_none()
            && [quota.five_hour.percent_used, quota.weekly.percent_used]
                .into_iter()
                .flatten()
                .any(|percent| percent >= 100.0)
    })
}

fn notify_exhausted(app: &AppHandle, account: &Account) {
    let reset = account
        .quota
        .as_ref()
        .and_then(|quota| {
            quota
                .five_hour
                .reset_at
                .clone()
                .or_else(|| quota.weekly.reset_at.clone())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let _ = app
        .notification()
        .builder()
        .title("Out of quota")
        .body(format!(
            "Account {} is out of quota, resets at {}",
            account.name, reset
        ))
        .show();
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Mechanism 2 prompts to extend when the 5h window has this many minutes (or fewer) left.
const EXTEND_THRESHOLD_MIN: i64 = 30;

/// One unit of work for the prime scheduler.
struct PrimeJob {
    tool_id: ToolId,
    account_id: String,
    account_name: String,
    config_dir: std::path::PathBuf,
    claude_binary: Option<std::path::PathBuf>,
    /// True when this job was triggered by an on-demand extend request (mechanism 2) rather than
    /// the scheduled time — used to clear the request on success.
    is_extend: bool,
}

/// True if the account exists and is prime-eligible (subscription Claude/Codex).
fn prime_eligible_in_state(data: &StoredState, tool_id: &ToolId, account_id: &str) -> bool {
    data.accounts.iter().any(|a| {
        a.id == account_id
            && a.tool_id == *tool_id
            && crate::prime::is_prime_eligible(&a.tool_id, a.api_provider.is_some())
    })
}

/// Parse a `HH:MM` 24h string into `(hour, minute)`, validating ranges.
fn parse_hhmm(input: &str) -> Option<(u32, u32)> {
    let (h, m) = input.trim().split_once(':')?;
    let hour: u32 = h.parse().ok()?;
    let minute: u32 = m.parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

/// Validate + normalize a `HH:MM` 24h string. Returns `Some("HH:MM")` (zero-padded) or None.
fn normalize_hhmm(input: &str) -> Option<String> {
    let (hour, minute) = parse_hhmm(input)?;
    Some(format!("{hour:02}:{minute:02}"))
}

/// Local date `YYYY-MM-DD` (machine timezone) — the "once per day" key.
fn local_today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Local time `HH:MM` (machine timezone) — compared against the scheduled prime time.
fn local_now_hhmm() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// Local timestamp for log lines: `YYYY-MM-DD HH:MM:SS` (machine timezone).
fn local_log_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Minutes from now until an ISO 8601 instant. Negative if already past; `i64::MAX` if unparseable.
fn minutes_until(iso: &str) -> i64 {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(t) => (t.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_minutes(),
        Err(_) => i64::MAX,
    }
}

/// Render an ISO 8601 instant as local `HH:MM` for log lines. Falls back to the raw string.
fn local_hhmm_from_iso(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| iso.to_string())
}

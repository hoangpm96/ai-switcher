use crate::models::{
    Account, AccountState, AddAccountInput, AddApiAccountInput, ApiGatewayAccount, ApiGatewayCombo,
    ApiGatewayConfig, ApiGatewayKey, ApiGatewayServerState, ApiGatewaySnapshot, ApiProvider,
    ApiUsageReport, AppSnapshot, AutoSwitchSetting, ClaudeOrgRecord, CreateApiGatewayKeyInput,
    CreateApiGatewayKeyResult, CreateVirtualApiAccountInput, DeleteApiGatewayComboInput,
    DeleteApiGatewayKeyInput, DetectionReport, OverlayRect, OverlaySettings, QuotaInfo,
    RenameAccountInput, SaveApiGatewayComboInput, SetAccountHiddenInput, SetApiGatewayAccountInput,
    SetLauncherInput, SetToolSetupInput, SetWeeklyLockInput, StartApiGatewayInput,
    SwitchAccountInput, ToolId, ToolStatus, UsageOrgLabel, UsageReport, WeeklyLock,
};
use crate::quota::{read_claude_profile, read_quota, ClaudeProfileIdentity};
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

/// Re-verify an account's Claude org at most this often — a profile dir can be re-logged into a
/// different subscription, after which new sessions mark usage with a different `credential_org`.
const CLAUDE_ORG_RECHECK: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);
/// Backoff after a failed `/api/oauth/profile` lookup (logged out, expired token, offline) so a
/// broken account isn't retried on every usage scan.
const CLAUDE_ORG_FAIL_RETRY: std::time::Duration = std::time::Duration::from_secs(30 * 60);
/// account_id → last successful org lookup in THIS process (the registry itself persists lookups
/// across restarts, so per-process timestamps are enough).
static CLAUDE_ORG_LAST_OK: Mutex<std::collections::BTreeMap<String, std::time::Instant>> =
    Mutex::new(std::collections::BTreeMap::new());
/// Set while `resolve_claude_orgs` runs, so overlapping usage reports don't duplicate lookups.
static CLAUDE_ORG_RESOLVING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// account_id → last failed org lookup, for the retry backoff above.
static CLAUDE_ORG_LAST_FAIL: Mutex<std::collections::BTreeMap<String, std::time::Instant>> =
    Mutex::new(std::collections::BTreeMap::new());

pub struct ManagedState {
    pub store: Store,
    pub data: Mutex<StoredState>,
    pub api_server: Mutex<crate::api_gateway::ApiServerHandle>,
    /// Serializes usage scans so repeated tab opens cannot race while rebuilding the same cache.
    pub usage_scan: Mutex<()>,
    /// True while a "Prime ngay" attempt is running, so a second button press can't start an
    /// overlapping attempt (send + confirm can block for ~2 minutes). An `Arc` so the backgrounded
    /// worker can move a clear-on-drop guard into its thread without a raw pointer (see
    /// `PrimingGuard`).
    pub priming: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
            usage_scan: Mutex::new(()),
            priming: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        for tool_id in [ToolId::Claude, ToolId::Codex, ToolId::Cursor, ToolId::Opencode] {
            let valid_dirs: Vec<std::path::PathBuf> = data
                .accounts
                .iter()
                .filter(|a| a.tool_id == tool_id && !a.is_default && !a.hidden)
                .map(|a| self.store.account_dir(&tool_id, &a.id))
                .collect();

            // Seed the onboarding flag + link the shared session for every profile (idempotent)
            // — ensures accounts logged in from a previous session also skip the wizard and
            // share the session store with Default. Hidden accounts keep healthy profiles so
            // unhiding them later still shares config/sessions.
            if matches!(tool_id, ToolId::Claude | ToolId::Codex) {
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

            // NOTE: orphan profile dirs (a folder under accounts/{tool}/ with no matching account)
            // are deliberately NOT auto-deleted. A dir can be missing from our account list yet still
            // be a LIVE Claude/Codex profile that another CLI session uses directly via
            // CLAUDE_CONFIG_DIR/CODEX_HOME (verified: such a dir kept being recreated mid-session with
            // fresh transcripts). Silently `remove_dir_all`-ing it would destroy that session's data.
            // Cleanup is now opt-in via the "Clean up old account data" action, which surfaces each
            // dir's size and an in-use warning before the user confirms.
        }
        if changed {
            let _ = install_shell_hook(&self.store);
        }
    }

    /// List leftover profile directories under `accounts/{tool}/` that belong to no current account.
    /// Read-only: it never deletes. Each entry carries its on-disk size and an `in_use` flag (a
    /// recently-modified transcript ⇒ a live CLI session is probably using it) so the UI can warn.
    pub fn list_orphan_account_dirs(&self) -> Result<Vec<crate::models::OrphanAccountDir>> {
        let data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        let mut orphans = Vec::new();
        for tool_id in [ToolId::Claude, ToolId::Codex] {
            // Dirs we must never offer to delete: every managed account's profile dir, plus the
            // configured default config dir if it happens to live under the tool root.
            let mut protected: Vec<std::path::PathBuf> = data
                .accounts
                .iter()
                .filter(|a| a.tool_id == tool_id && !a.is_default)
                .map(|a| self.store.account_dir(&tool_id, &a.id))
                .collect();
            if let Some(default_dir) = configured_default_config_dir(&data, &tool_id) {
                protected.push(default_dir);
            }
            let tool_root = self.store.account_dir(&tool_id, "");
            let Ok(entries) = std::fs::read_dir(&tool_root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || protected.contains(&path) {
                    continue;
                }
                let size_bytes = dir_size_bytes(&path);
                orphans.push(crate::models::OrphanAccountDir {
                    tool_id: tool_id.clone(),
                    id: entry.file_name().to_string_lossy().to_string(),
                    path: path.to_string_lossy().to_string(),
                    size_bytes,
                    size_label: human_size(size_bytes),
                    in_use: dir_recently_active(&path),
                });
            }
        }
        Ok(orphans)
    }

    /// Delete one orphan profile directory, identified by its directory NAME (the former account id),
    /// not a full path — so a crafted absolute path or `..` can never escape the tool root. The name
    /// must be a single, non-traversal component; the resolved dir must not belong to a managed
    /// account or be the configured default config dir.
    pub fn delete_orphan_account_dir(&self, tool_id: ToolId, id: String) -> Result<()> {
        // A profile dir name is a single path component (an account UUID). Reject anything that could
        // traverse: separators, `.`/`..`, or an absolute/parent-bearing path.
        if id.is_empty()
            || id == "."
            || id == ".."
            || id.contains('/')
            || id.contains('\\')
            || std::path::Path::new(&id).components().count() != 1
        {
            anyhow::bail!("ID thư mục không hợp lệ.");
        }
        let target = self.store.account_dir(&tool_id, &id);
        {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let is_managed = data.accounts.iter().any(|a| {
                a.tool_id == tool_id
                    && !a.is_default
                    && self.store.account_dir(&tool_id, &a.id) == target
            });
            let is_default = configured_default_config_dir(&data, &tool_id)
                .is_some_and(|default_dir| default_dir == target);
            if is_managed || is_default {
                anyhow::bail!("Thư mục này đang được sử dụng — không xóa.");
            }
        }
        if !target.is_dir() {
            anyhow::bail!("Không tìm thấy thư mục.");
        }
        std::fs::remove_dir_all(&target)
            .map_err(|e| anyhow::anyhow!("Không xóa được thư mục: {e}"))?;
        Ok(())
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
            let model = match input
                .model
                .as_deref()
                .map(str::trim)
                .filter(|m| !m.is_empty())
            {
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
                    .context(
                        "Create at least one combo or pick a model before adding the account",
                    )?,
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
            other => unreachable!("{} is guarded above", other.as_str()),
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
                _ => {}
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
                    hidden: false,
                    weekly_lock: None,
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
    pub fn set_api_gateway_account(&self, input: SetApiGatewayAccountInput) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            match data.api_gateway.accounts.iter_mut().find(|entry| {
                entry.tool_id == input.tool_id && entry.account_id == input.account_id
            }) {
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
                        && !account.hidden
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
        // An app-managed profile dir must never become the tool's default config dir: every other
        // account symlinks its shared config/sessions back into the default dir, so pointing the
        // default at profile B means deleting account B breaks all of them.
        let app_root = canonical_path(&self.store.tool_accounts_root(&input.tool_id));
        if canonical_path(&input.default_config_dir).starts_with(&app_root) {
            anyhow::bail!(
                "That folder is a profile this app manages — pick the tool's own config folder (e.g. ~/.claude, ~/.codex) instead"
            );
        }

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
        // Bring the org registry up to date first so marker-attributed usage lands under the
        // right label. Throttled per account — normally a no-op. Runs before taking the scan lock
        // so its HTTP calls never hold up a concurrent report.
        self.resolve_claude_orgs();
        let _scan = self
            .usage_scan
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let org_labels = self
            .data
            .lock()
            .map(|data| claude_org_labels(&data.claude_orgs, &data.accounts))
            .unwrap_or_default();
        crate::usage::build_report(
            &self.store.usage_cache_path(),
            &self.store.price_cache_path(),
            &self.config_dirs(&ToolId::Claude),
            &self.config_dirs(&ToolId::Codex),
            range_days,
            &org_labels,
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

    /// Refresh the persistent Claude-org registry (`data.claude_orgs`): for every eligible Claude
    /// account that isn't in the registry yet — or hasn't been verified recently — fetch
    /// `/api/oauth/profile` and record which organization (= `credential_org` marker uuid) the
    /// login belongs to. Best-effort like a quota refresh: failures are throttled and swallowed,
    /// never surfaced to the caller.
    fn resolve_claude_orgs(&self) {
        // One lookup round at a time: a second report arriving mid-round just uses the registry
        // as it stands instead of firing the same HTTP calls again.
        if CLAUDE_ORG_RESOLVING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        struct Release;
        impl Drop for Release {
            fn drop(&mut self) {
                CLAUDE_ORG_RESOLVING.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        let _release = Release;
        // Phase 1: pick accounts needing a lookup — brief lock, no I/O.
        let targets: Vec<(String, String, std::path::PathBuf)> = {
            let Ok(data) = self.data.lock() else {
                return;
            };
            let last_ok = CLAUDE_ORG_LAST_OK
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let last_fail = CLAUDE_ORG_LAST_FAIL
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default();
            let default_dir = resolved_default_config_dir(&data, &ToolId::Claude);
            data.accounts
                .iter()
                .filter(|account| {
                    account.tool_id == ToolId::Claude
                        && account.api_provider.is_none()
                        && !is_virtual_api_account(account)
                })
                .filter(|account| {
                    let unregistered = !data.claude_orgs.values().any(|record| {
                        record.account_ids.iter().any(|id| id == &account.id)
                    });
                    let stale = last_ok
                        .get(&account.id)
                        .is_none_or(|instant| instant.elapsed() >= CLAUDE_ORG_RECHECK);
                    if !unregistered && !stale {
                        return false;
                    }
                    last_fail
                        .get(&account.id)
                        .is_none_or(|instant| instant.elapsed() >= CLAUDE_ORG_FAIL_RETRY)
                })
                .map(|account| {
                    (
                        account.id.clone(),
                        account.name.clone(),
                        account_config_dir_with_default(&self.store, account, &default_dir),
                    )
                })
                .collect()
        };
        if targets.is_empty() {
            return;
        }

        // Phase 2: fetch profiles in parallel — no mutex held.
        let results: Vec<_> = {
            let handles: Vec<_> = targets
                .into_iter()
                .map(|(account_id, account_name, config_dir)| {
                    std::thread::spawn(move || {
                        let identity = read_claude_profile(&config_dir);
                        (account_id, account_name, identity)
                    })
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        };

        // Record outcomes for the recheck/failure windows before touching the data lock.
        if let (Ok(mut last_ok), Ok(mut last_fail)) =
            (CLAUDE_ORG_LAST_OK.lock(), CLAUDE_ORG_LAST_FAIL.lock())
        {
            let instant = std::time::Instant::now();
            for (account_id, _, result) in &results {
                if result.is_ok() {
                    last_ok.insert(account_id.clone(), instant);
                    last_fail.remove(account_id);
                } else {
                    last_fail.insert(account_id.clone(), instant);
                }
            }
        }

        // Phase 3: upsert into the registry + save once — brief lock.
        let Ok(mut data) = self.data.lock() else {
            return;
        };
        let timestamp = now();
        let mut changed = false;
        for (account_id, name, result) in results {
            let Ok(identity) = result else {
                continue;
            };
            // The account may have been renamed or deleted while the HTTP calls were in flight;
            // prefer the live name, fall back to the one captured in Phase 1.
            let account_name = data
                .accounts
                .iter()
                .find(|account| account.id == account_id)
                .map(|account| account.name.clone())
                .unwrap_or(name);
            changed |= upsert_claude_org(
                &mut data.claude_orgs,
                &identity,
                &account_id,
                &account_name,
                &timestamp,
            );
        }
        if changed {
            let _ = self.store.save(&data);
        }
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
                .filter(|a| a.state == AccountState::NeedsLogin && !a.is_default && !a.hidden)
                .map(|a| (a.tool_id.clone(), a.id.clone()))
                .collect()
        };
        for (tool_id, account_id) in pending {
            let _ = self.confirm_login(&tool_id, &account_id);
        }
        Ok(())
    }

    pub fn refresh_tool(&self, tool_id: ToolId, app: Option<&AppHandle>) -> Result<AppSnapshot> {
        // Phase 1: collect (account_id, config_dir) — brief lock, no HTTP, no disk I/O.
        // Also collect the dirs that may need their shared session link healed; the filesystem work
        // runs AFTER the lock is dropped so a slow disk never blocks the UI or other commands.
        let (accounts_info, heal_targets): (
            Vec<(String, std::path::PathBuf)>,
            Vec<std::path::PathBuf>,
        ) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let default_config_dir = resolved_default_config_dir(&data, &tool_id);
            let heal_targets = data
                .accounts
                .iter()
                .filter(|a| a.tool_id == tool_id && !a.is_default && a.api_provider.is_none())
                .map(|a| self.store.account_dir(&tool_id, &a.id))
                .collect();
            let accounts_info = data
                .accounts
                .iter()
                .filter(|a| {
                    a.tool_id == tool_id
                        && !a.hidden
                        && a.state != AccountState::NeedsLogin
                        && a.api_provider.is_none()
                })
                .map(|a| {
                    (
                        a.id.clone(),
                        account_config_dir_with_default(&self.store, a, &default_config_dir),
                    )
                })
                .collect();
            (accounts_info, heal_targets)
        };
        // Mutex released — disk I/O + HTTP calls run without blocking other operations.

        // Self-heal the shared session link for each managed account (outside the lock). An account
        // whose `projects/` (Claude) / `sessions/` (Codex) is still a real directory — created but
        // never switched into — keeps its memory/transcripts to itself instead of the shared store,
        // so memory saved while using it is invisible to other accounts. `link_shared_sessions_to` is
        // idempotent (a no-op once the symlink already points at the shared target), so this cheaply
        // guarantees a newly-added account shares memory without waiting for a switch, login, or app
        // restart. (Config links stay on the login/heal paths — they don't carry memory and shouldn't
        // churn on every periodic refresh.)
        if let Some(configured_default) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            configured_default_config_dir(&data, &tool_id)
        } {
            for dir in &heal_targets {
                crate::tools::link_shared_sessions_to(&tool_id, dir, &configured_default);
            }
        }

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
        let (exhausted_accounts, lock_changes): (Vec<Account>, Vec<(String, bool)>) = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let timestamp = now();
            let mut exhausted = Vec::new();
            let mut lock_changes = Vec::new();
            for (account_id, quota) in results {
                if let Some(account) = data
                    .accounts
                    .iter_mut()
                    .find(|a| a.tool_id == tool_id && a.id == account_id)
                {
                    let was_exhausted = account.state == AccountState::Exhausted;
                    account.quota = Some(keep_last_quota_when_rate_limited(
                        account.quota.as_ref(),
                        quota,
                    ));
                    account.updated_at = timestamp.clone();
                    account.state = if is_exhausted(account) {
                        AccountState::Exhausted
                    } else if was_exhausted {
                        AccountState::Idle
                    } else {
                        account.state.clone()
                    };
                    // Notify only on the TRANSITION into exhausted, not on every refresh while it
                    // stays exhausted — otherwise the 5-minute poller fires the same notification
                    // over and over until the window resets.
                    if account.state == AccountState::Exhausted && !was_exhausted {
                        exhausted.push(account.clone());
                    }
                    if let Some(lock) = weekly_lock_transition(account) {
                        lock_changes.push((account_id, lock));
                    }
                }
            }
            self.store.save(&data)?;
            (exhausted, lock_changes)
        };

        if let Some(app) = app {
            for account in &exhausted_accounts {
                notify_exhausted(app, account);
            }
        }
        self.apply_weekly_lock_changes(&tool_id, lock_changes, app);

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
                        && !a.hidden
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
        let (exhausted, lock_change): (Option<Account>, Option<bool>) = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let mut result = None;
            let mut lock_change = None;
            if let Some(account) = data
                .accounts
                .iter_mut()
                .find(|a| a.tool_id == *tool_id && a.id == account_id)
            {
                let was_exhausted = account.state == AccountState::Exhausted;
                account.quota = Some(keep_last_quota_when_rate_limited(
                    account.quota.as_ref(),
                    quota,
                ));
                account.updated_at = now();
                account.state = if is_exhausted(account) {
                    AccountState::Exhausted
                } else if was_exhausted {
                    AccountState::Idle
                } else {
                    account.state.clone()
                };
                // Notify only on the transition INTO exhausted (see refresh_tool) — not on every
                // single-account refresh while it stays exhausted.
                if account.state == AccountState::Exhausted && !was_exhausted {
                    result = Some(account.clone());
                }
                lock_change = weekly_lock_transition(account);
            }
            self.store.save(&data)?;
            (result, lock_change)
        };

        if let (Some(app), Some(account)) = (app, exhausted) {
            notify_exhausted(app, &account);
        }
        let lock_changes = lock_change
            .map(|lock| vec![(account_id.to_string(), lock)])
            .unwrap_or_default();
        self.apply_weekly_lock_changes(tool_id, lock_changes, app);

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

    /// Current settings of the always-on-top quota overlay.
    pub fn overlay_settings(&self) -> Result<OverlaySettings> {
        let data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        Ok(data.overlay.clone())
    }

    /// Persist overlay settings coming from the UI. Values are clamped here (not in the UI) so a
    /// hand-edited `state.json` can't produce an invisible or off-screen overlay.
    pub fn save_overlay_settings(&self, mut next: OverlaySettings) -> Result<OverlaySettings> {
        next.opacity = clamp_opacity(next.opacity, 0.45);
        next.hover_opacity = clamp_opacity(next.hover_opacity, 1.0);
        // Order matters (it's the row order), so keep the first occurrence of each key.
        let mut seen = std::collections::HashSet::new();
        next.accounts.retain(|key| seen.insert(key.clone()));
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        // Geometry is owned by the window-move/resize handler, not by the settings form.
        next.rect = data.overlay.rect;
        data.overlay = next.clone();
        self.store.save(&data)?;
        Ok(next)
    }

    /// Remember where the user dragged/resized the overlay to. Called on every move/resize event,
    /// so it only touches disk when the geometry actually changed.
    pub fn set_overlay_rect(&self, rect: OverlayRect) -> Result<()> {
        if !(rect.width.is_finite() && rect.height.is_finite() && rect.x.is_finite() && rect.y.is_finite()) {
            return Ok(());
        }
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        let current = data.overlay.rect;
        let same = (current.x - rect.x).abs() < 1.0
            && (current.y - rect.y).abs() < 1.0
            && (current.width - rect.width).abs() < 1.0
            && (current.height - rect.height).abs() < 1.0;
        if same {
            return Ok(());
        }
        data.overlay.rect = rect;
        self.store.save(&data)
    }

    /// Flip only the `enabled` flag (tray toggle / overlay close button) and return the new value.
    pub fn set_overlay_enabled(&self, enabled: bool) -> Result<OverlaySettings> {
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        data.overlay.enabled = enabled;
        self.store.save(&data)?;
        Ok(data.overlay.clone())
    }

    /// Prepend one event line to the prime activity log so the NEWEST entry is at the TOP of the
    /// file (no scrolling to the bottom to see what just happened). The log is small, so rewriting
    /// it on each append is cheap. Written atomically (temp + rename) so a concurrent read never
    /// sees a half-written file.
    fn append_prime_log(&self, line: &str) {
        let _ = self.prepend_prime_log_line(line);
    }

    /// Write `line` (timestamped) at the TOP of the auto-prime log, keeping the rest below it.
    fn prepend_prime_log_line(&self, line: &str) -> Result<()> {
        let path = self.store.auto_prime_log_path();
        let stamped = format!("[{}] {}\n", local_log_timestamp(), line);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let temporary = path.with_extension("log.tmp");
        std::fs::write(&temporary, format!("{stamped}{existing}"))
            .context("writing auto-prime log")?;
        std::fs::rename(&temporary, &path).context("replacing auto-prime log")?;
        Ok(())
    }

    /// Handle the "Làm mới token" button for a Claude account whose stored access token has expired.
    ///
    /// This used to spawn the account's `claude` CLI in the background (`-p hi`) on the theory that
    /// the CLI's own refresh is "session-safe" and can never invalidate a live/overnight CLI session.
    /// Verified live 2026-07-08/09: that's false — a background CLI-refresh run left an interactive
    /// terminal session on the SAME account (an isolated app-managed profile dir, not the shared
    /// default `~/.claude`) unable to log back in, forcing a manual `/login` even in a brand-new
    /// terminal window. The app now NEVER spawns the CLI to refresh a token, on demand or scheduled
    /// (see `prime::prime_account_traced`'s D1) — the only proven-safe way to renew is for you to
    /// open that account's `claude` CLI yourself. This handler just re-reads quota (covers the
    /// already-valid case) and otherwise tells you to log in manually.
    pub fn refresh_token_now(
        &self,
        tool_id: ToolId,
        account_id: String,
        app: Option<&AppHandle>,
    ) -> Result<crate::models::PrimeNowResult> {
        // Resolve the account's config dir under a brief lock; reject non-Claude / API.
        let config_dir = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|a| a.id == account_id && a.tool_id == tool_id)
                .ok_or_else(|| anyhow::anyhow!("Không tìm thấy tài khoản"))?;
            if account.hidden {
                anyhow::bail!("This account is hidden — unhide it first");
            }
            // Token renewal only applies to Claude subscription (OAuth) accounts. Codex owns its own
            // ~8-day token lifecycle, and API-proxy accounts have no OAuth token to refresh.
            if account.tool_id != ToolId::Claude || account.api_provider.is_some() {
                anyhow::bail!("Chỉ làm mới được token cho tài khoản Claude đăng nhập subscription");
            }
            let default_dir = resolved_default_config_dir(&data, &account.tool_id);
            account_config_dir_with_default(&self.store, account, &default_dir)
        };

        use crate::models::PrimeNowResult;
        match crate::quota::claude_token_state(&config_dir) {
            crate::quota::ClaudeTokenState::Missing => Ok(PrimeNowResult {
                kind: "error".to_string(),
                message: "Tài khoản chưa đăng nhập. Mở Claude Code để đăng nhập lại.".to_string(),
            }),
            // Token is already usable — don't spawn the CLI (it would send a tiny request that costs
            // quota). Just re-read quota so the card clears a transient 401.
            crate::quota::ClaudeTokenState::Valid => {
                let _ = self.refresh_tool(ToolId::Claude, app);
                Ok(PrimeNowResult {
                    kind: "success".to_string(),
                    message: "Token vẫn còn hạn. Đang cập nhật lại quota…".to_string(),
                })
            }
            crate::quota::ClaudeTokenState::Expired => Ok(PrimeNowResult {
                kind: "error".to_string(),
                message: "Token đã hết hạn. App không tự làm mới được (tránh làm văng phiên `claude` đang chạy trên account này) — hãy mở terminal, chạy `claude` trên account này và đăng nhập lại thủ công.".to_string(),
            }),
        }
    }

    /// Prime ONE account on demand ("Prime ngay" button) — open a fresh 5h window right now. One
    /// bounded attempt per press: send once over HTTP, poll briefly to confirm, report the result.
    /// No background retries and no durable state — if the attempt doesn't confirm, the user simply
    /// presses the button again. The `priming` overlap guard stops a second press from running on
    /// top of an in-flight attempt. Returns a short human message surfaced to the UI as a toast.
    pub fn prime_now(
        &self,
        tool_id: ToolId,
        account_id: String,
        app: Option<&AppHandle>,
    ) -> Result<crate::models::PrimeNowResult> {
        use std::sync::atomic::Ordering;

        // Build the job under a brief lock; reject early if the account isn't prime-eligible.
        let job = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|a| a.id == account_id && a.tool_id == tool_id)
                .ok_or_else(|| anyhow::anyhow!("Không tìm thấy tài khoản"))?;
            if account.hidden {
                anyhow::bail!("This account is hidden — unhide it first");
            }
            if account.is_locked() {
                anyhow::bail!("This account is locked to save its weekly quota — unlock it first");
            }
            if !crate::prime::is_prime_eligible(&account.tool_id, account.api_provider.is_some()) {
                anyhow::bail!(
                    "Tài khoản này không hỗ trợ prime (chỉ Claude/Codex đăng nhập subscription)"
                );
            }
            let default_dir = resolved_default_config_dir(&data, &account.tool_id);
            let config_dir = account_config_dir_with_default(&self.store, account, &default_dir);
            PrimeJob {
                tool_id: account.tool_id.clone(),
                account_id: account.id.clone(),
                account_name: account.name.clone(),
                config_dir,
            }
        };

        // Don't run on top of another in-flight manual prime.
        if self
            .priming
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            anyhow::bail!("Đang có một lượt prime khác chạy — thử lại sau giây lát.");
        }
        // The overlap guard holds a clone of the `Arc<AtomicBool>`, so it can move into the background
        // worker and outlive this function. `priming` is reset exactly once when the guard drops — on
        // an early return here, or when the worker thread finishes.
        let _guard = PrimingGuard::new(self);

        // The send+confirm step blocks for up to ~2 minutes (the confirm poll). Run it on a
        // background thread so the button returns immediately; the final result is delivered via the
        // `prime-now-done` event. The synchronous fallback (no app handle — tests) keeps the original
        // blocking behaviour.
        if let Some(app) = app {
            let app = app.clone();
            let account_id = job.account_id.clone();
            std::thread::spawn(move || {
                let _guard = _guard;
                let state = app.state::<ManagedState>();
                let result = state.run_manual_prime(&job, Some(&app));
                let _ = app.emit(
                    "prime-now-done",
                    crate::models::PrimeNowDone {
                        account_id,
                        kind: result.kind,
                        message: result.message,
                    },
                );
            });
            return Ok(crate::models::PrimeNowResult {
                kind: "pending".to_string(),
                message: "Đang mở phiên mới…".to_string(),
            });
        }

        Ok(self.run_manual_prime(&job, None))
    }

    /// Blocking core of a manual prime: send one lightweight request, confirm, log the outcome and
    /// refresh the account's quota on success. Split out so `prime_now` can run it on a background
    /// thread (GUI) or inline (tests). The caller holds the overlap guard for the duration.
    fn run_manual_prime(&self, job: &PrimeJob, app: Option<&AppHandle>) -> crate::models::PrimeNowResult {
        use crate::prime::PrimeOutcome;

        let trace_prefix = format!(
            "[PRIME NGAY] {} · account \"{}\" —",
            job.tool_id.prime_label(),
            job.account_name,
        );
        let trace = |line: &str| self.append_prime_log(&format!("{trace_prefix} {line}"));
        let outcome = crate::prime::prime_account_traced(
            &job.tool_id,
            &job.config_dir,
            std::thread::sleep,
            trace,
        );

        // On success, refresh the displayed quota right away so the card shows the new reset.
        if matches!(outcome, PrimeOutcome::Success { .. }) {
            let _ = self.refresh_single_account(&job.tool_id, &job.account_id, app);
        }

        let (kind, message) = match &outcome {
            PrimeOutcome::Success { new_reset_at } if matches!(job.tool_id, ToolId::Codex) => (
                "success",
                format!(
                    "Session Codex đã được xác nhận — reset lúc {}.",
                    local_hhmm_from_iso(new_reset_at)
                ),
            ),
            PrimeOutcome::Success { new_reset_at } => (
                "success",
                format!("Đã mở phiên mới — reset lúc {}", local_hhmm_from_iso(new_reset_at)),
            ),
            // Not an error: the window is simply still running (or was just refreshed by another
            // account that shares this ChatGPT/Claude login), so a new one can't open yet.
            PrimeOutcome::Hold { reset_at } => (
                "info",
                format!(
                    "Phiên hiện tại vẫn còn (đến {}). Phiên mới chỉ mở được sau khi phiên này kết thúc.",
                    local_hhmm_from_iso(reset_at)
                ),
            ),
            PrimeOutcome::SkipNoToken => (
                "error",
                "Token chưa sẵn sàng (hết hạn hoặc chưa đăng nhập). Mở CLI của account này để đăng nhập lại rồi bấm lại.".to_string(),
            ),
            PrimeOutcome::SkipUnknownState => (
                "info",
                "Chưa đọc được trạng thái phiên hiện tại nên không gửi — thử lại sau giây lát.".to_string(),
            ),
            PrimeOutcome::FailSend { reason } => (
                "error",
                format!("Gửi yêu cầu prime lỗi ({reason}) — bấm lại để thử lần nữa."),
            ),
            // The send itself succeeded; only the confirmation poll ran out. Codex in particular can
            // anchor with a delay — tell the user to re-check instead of claiming failure.
            PrimeOutcome::FailUnconfirmed => (
                "info",
                "Đã gửi yêu cầu nhưng chưa xác nhận được reset mới trong thời gian chờ. Chờ một lát rồi bấm Refresh quota — nếu reset chưa đổi, bấm Prime ngay lần nữa.".to_string(),
            ),
        };
        crate::models::PrimeNowResult {
            kind: kind.to_string(),
            message: message.to_string(),
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
        let target = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            // Resolve the account in use from the active-profile file, NOT from `state == Active`:
            // an account that just hit 100% is marked `Exhausted`, so matching on `Active` misses
            // exactly the case auto-switch exists for.
            let active_id = active_account_id_for(&self.store, tool_id, &data.accounts);
            let active = active_id.as_deref().and_then(|id| {
                data.accounts
                    .iter()
                    .find(|a| a.tool_id == *tool_id && a.id == id)
            });
            // Hidden / weekly-locked accounts are off limits — leave them if the bare command still
            // points there. Otherwise only switch when the account in use has hit the threshold.
            let replacement = match active {
                Some(active) if active.hidden || active.is_locked() => {
                    best_replacement(&data.accounts, tool_id, threshold, Some(&active.id))
                }
                Some(active) if max_percent_used(active) >= threshold => {
                    best_replacement(&data.accounts, tool_id, threshold, Some(&active.id))
                }
                _ => None,
            };
            replacement.map(|account| (account.id.clone(), account.is_default))
        };

        let Some((target_id, target_is_default)) = target else {
            return Ok(());
        };

        if target_is_default {
            clear_active_profile(tool_id, &self.store)
                .context("Auto-switch failed while clearing the active profile")?;
        } else {
            write_active_profile(tool_id, &self.store, &target_id)
                .context("Auto-switch failed while writing the active profile")?;
        }
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
                hidden: false,
                weekly_lock: None,
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
                hidden: false,
                weekly_lock: None,
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
            other => unreachable!("{} is guarded above", other.as_str()),
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
                hidden: false,
                weekly_lock: None,
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
            account.name = name.clone();
            account.updated_at = now();
            // Keep the org registry's stored name fresh too — usage labels prefer the live name
            // anyway, but the record is what survives if the account is later deleted.
            for record in data.claude_orgs.values_mut() {
                if let Some(index) = record
                    .account_ids
                    .iter()
                    .position(|id| id == &input.account_id)
                {
                    if index < record.account_names.len() {
                        record.account_names[index] = name.clone();
                    }
                }
            }
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
            if account.hidden {
                anyhow::bail!("This account is hidden — unhide it first");
            }
            if account.is_locked() {
                anyhow::bail!("This account is locked to save its weekly quota — unlock it first");
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
            let _ = remove_launcher(&old);
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

    /// Hide an account (treat as not added: drop launcher, leave the list, cannot be selected)
    /// or unhide it (restore launcher, back on the list). Profile/credentials stay on disk.
    pub fn set_account_hidden(&self, input: SetAccountHiddenInput) -> Result<AppSnapshot> {
        if input.hidden {
            self.hide_account(&input.tool_id, &input.account_id)
        } else {
            self.unhide_account(&input.tool_id, &input.account_id)
        }
    }

    fn hide_account(&self, tool_id: &ToolId, account_id: &str) -> Result<AppSnapshot> {
        let (launcher, was_active) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|a| a.tool_id == *tool_id && a.id == account_id)
                .context("Account not found")?;
            if account.is_default {
                anyhow::bail!("Can't hide the machine default account");
            }
            if is_virtual_api_account(account) {
                anyhow::bail!("Local API accounts are managed from the API tab");
            }
            if account.hidden {
                drop(data);
                return self.snapshot();
            }
            let was_active =
                active_account_id_for(&self.store, tool_id, &data.accounts).as_deref()
                    == Some(account_id);
            (account.launcher_command.clone(), was_active)
        };

        // The bare command must not keep pointing at a hidden profile.
        if was_active
            && matches!(
                tool_id,
                ToolId::Claude | ToolId::Codex | ToolId::Cursor | ToolId::Opencode
            )
        {
            clear_active_profile(tool_id, &self.store)
                .context("Couldn't clear the account in use — nothing was hidden")?;
            install_shell_hook(&self.store)
                .context("Couldn't update the shell hook — nothing was hidden")?;
        }

        if let Some(name) = launcher {
            remove_launcher(&name)
                .context("Couldn't remove the account's command — nothing was hidden")?;
        }

        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let overlay_key = format!("{}:{account_id}", tool_id.as_str());
            data.overlay.accounts.retain(|key| key != &overlay_key);
            if was_active {
                let default_id = data
                    .accounts
                    .iter()
                    .find(|a| a.tool_id == *tool_id && a.is_default)
                    .map(|a| a.id.clone());
                normalize_account_states(&mut data.accounts, tool_id, default_id.as_deref());
            }
            if let Some(account) = data
                .accounts
                .iter_mut()
                .find(|a| a.tool_id == *tool_id && a.id == account_id)
            {
                account.hidden = true;
                account.updated_at = now();
            }
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    fn unhide_account(&self, tool_id: &ToolId, account_id: &str) -> Result<AppSnapshot> {
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        let restored = data
            .accounts
            .iter()
            .find(|a| a.tool_id == *tool_id && a.id == account_id)
            .cloned()
            .context("Account not found")?;
        if !restored.hidden {
            drop(data);
            return self.snapshot();
        }
        if !restored.is_locked() {
            restore_launcher_file(&restored, &self.store, &data)
                .context("Couldn't restore the account's command — it stayed hidden")?;
        }
        if let Some(account) = data
            .accounts
            .iter_mut()
            .find(|a| a.tool_id == *tool_id && a.id == account_id)
        {
            account.hidden = false;
            account.updated_at = now();
        }
        self.store.save(&data)?;
        drop(data);
        self.snapshot()
    }

    /// Arm / disarm the weekly reserve-quota lock. Disarming unlocks the account (manual unlock);
    /// arming checks the current weekly usage right away, so it can lock immediately.
    pub fn set_weekly_lock(&self, input: SetWeeklyLockInput) -> Result<AppSnapshot> {
        if !input.threshold.is_finite() || !(1.0..=100.0).contains(&input.threshold) {
            anyhow::bail!("The weekly limit must be between 1% and 100%");
        }
        let change = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter_mut()
                .find(|a| a.tool_id == input.tool_id && a.id == input.account_id)
                .context("Account not found")?;
            if !supports_weekly_lock(&account.tool_id) {
                anyhow::bail!("This tool has no weekly limit to watch");
            }
            if account.is_default {
                anyhow::bail!("Can't lock the machine default account — the plain command falls back to it");
            }
            if account.api_provider.is_some() {
                anyhow::bail!("API accounts have no quota to watch");
            }
            let was_locked = account.is_locked();
            account.weekly_lock = Some(WeeklyLock {
                enabled: input.enabled,
                threshold: input.threshold,
                locked: was_locked,
            });
            account.updated_at = now();
            let change = if input.enabled {
                weekly_lock_transition(account)
            } else if was_locked {
                Some(false)
            } else {
                None
            };
            self.store.save(&data)?;
            change
        };
        match change {
            Some(true) => self.lock_account(&input.tool_id, &input.account_id)?,
            Some(false) => self.unlock_account(&input.tool_id, &input.account_id)?,
            None => {}
        }
        self.snapshot()
    }

    /// Apply the lock/unlock decisions from a quota refresh. Failures are reported but never fail
    /// the refresh itself — the next refresh retries.
    fn apply_weekly_lock_changes(
        &self,
        tool_id: &ToolId,
        changes: Vec<(String, bool)>,
        app: Option<&AppHandle>,
    ) {
        for (account_id, lock) in changes {
            let result = if lock {
                self.lock_account(tool_id, &account_id)
            } else {
                self.unlock_account(tool_id, &account_id)
            };
            if let Err(err) = result {
                eprintln!("weekly lock: couldn't update {account_id}: {err:#}");
                continue;
            }
            let Some(app) = app else { continue };
            let Some(account) = self.data.lock().ok().and_then(|data| {
                data.accounts
                    .iter()
                    .find(|a| a.tool_id == *tool_id && a.id == account_id)
                    .cloned()
            }) else {
                continue;
            };
            notify_weekly_lock(app, &account, lock);
        }
    }

    /// Lock: drop the launcher and move the plain command off this account. Profile and
    /// credentials stay; quota keeps refreshing so the weekly reset can unlock it.
    fn lock_account(&self, tool_id: &ToolId, account_id: &str) -> Result<()> {
        let (launcher, was_active, replacement) = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account = data
                .accounts
                .iter()
                .find(|a| a.tool_id == *tool_id && a.id == account_id)
                .context("Account not found")?;
            let was_active =
                active_account_id_for(&self.store, tool_id, &data.accounts).as_deref()
                    == Some(account_id);
            let replacement = if was_active {
                best_replacement(&data.accounts, tool_id, 100.0, Some(account_id))
                    .map(|a| (a.id.clone(), a.is_default))
            } else {
                None
            };
            (account.launcher_command.clone(), was_active, replacement)
        };

        // The plain command must not keep running a locked account: hand it to the healthiest
        // other account, or back to the machine default.
        let new_active_id = if was_active {
            match &replacement {
                Some((id, false)) => {
                    write_active_profile(tool_id, &self.store, id)
                        .context("Couldn't move the account in use — nothing was locked")?;
                }
                _ => clear_active_profile(tool_id, &self.store)
                    .context("Couldn't clear the account in use — nothing was locked")?,
            }
            install_shell_hook(&self.store)
                .context("Couldn't update the shell hook — nothing was locked")?;
            replacement.map(|(id, _)| id)
        } else {
            None
        };

        if let Some(name) = launcher {
            remove_launcher(&name)
                .context("Couldn't remove the account's command — nothing was locked")?;
        }

        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        if was_active {
            let active_id = new_active_id.or_else(|| {
                data.accounts
                    .iter()
                    .find(|a| a.tool_id == *tool_id && a.is_default)
                    .map(|a| a.id.clone())
            });
            normalize_account_states(&mut data.accounts, tool_id, active_id.as_deref());
        }
        if let Some(account) = data
            .accounts
            .iter_mut()
            .find(|a| a.tool_id == *tool_id && a.id == account_id)
        {
            if let Some(lock) = account.weekly_lock.as_mut() {
                lock.locked = true;
            }
            account.updated_at = now();
        }
        self.store.save(&data)?;
        Ok(())
    }

    /// Unlock: bring the launcher back (unless the account is also hidden).
    fn unlock_account(&self, tool_id: &ToolId, account_id: &str) -> Result<()> {
        let mut data = self
            .data
            .lock()
            .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
        let account = data
            .accounts
            .iter()
            .find(|a| a.tool_id == *tool_id && a.id == account_id)
            .cloned()
            .context("Account not found")?;
        if !account.hidden {
            restore_launcher_file(&account, &self.store, &data)
                .context("Couldn't restore the account's command — it stayed locked")?;
        }
        if let Some(account) = data
            .accounts
            .iter_mut()
            .find(|a| a.tool_id == *tool_id && a.id == account_id)
        {
            if let Some(lock) = account.weekly_lock.as_mut() {
                lock.locked = false;
            }
            account.updated_at = now();
        }
        self.store.save(&data)?;
        Ok(())
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
            if account.hidden {
                anyhow::bail!("This account is hidden — unhide it first");
            }
            if account.is_locked() {
                anyhow::bail!("This account is locked to save its weekly quota — unlock it first");
            }
            (account.is_default, account.state.clone())
        };

        if state == AccountState::NeedsLogin {
            anyhow::bail!("Account hasn't finished logging in yet");
        }

        match input.tool_id {
            // Every profile-based CLI switches the same way: point the active file at the chosen
            // profile (or clear it for the machine default) and refresh the shell hook.
            ToolId::Claude | ToolId::Codex | ToolId::Cursor | ToolId::Opencode => {
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
        let (launcher, was_active, pending_org_lookup) = {
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
            // "In use" comes from the active-profile file, not from `state`: an account that ran
            // out of quota is marked `Exhausted` while still being the one the plain command uses,
            // and deleting it without clearing the active file leaves that file pointing at a
            // profile dir that no longer exists.
            let was_active =
                active_account_id_for(&self.store, &tool_id, &data.accounts).as_deref()
                    == Some(account_id.as_str());
            // If this Claude OAuth account was never resolved to its org, remember enough to look
            // it up BEFORE the profile/credentials are removed — the registry then keeps its name
            // for usage the account already produced.
            let pending_org_lookup = if tool_id == ToolId::Claude
                && account.api_provider.is_none()
                && !is_virtual_api_account(account)
                && !data.claude_orgs.values().any(|record| {
                    record.account_ids.iter().any(|id| id == &account.id)
                })
            {
                let default_dir = resolved_default_config_dir(&data, &tool_id);
                Some((
                    account.name.clone(),
                    account_config_dir_with_default(&self.store, account, &default_dir),
                ))
            } else {
                None
            };
            (account.launcher_command.clone(), was_active, pending_org_lookup)
        };

        // Refuse if this account's folder is the tool's configured default config dir — other
        // accounts symlink their shared config/sessions into it, so removing it breaks them all.
        {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            let account_dir = canonical_path(&self.store.account_dir(&tool_id, &account_id));
            let configured = data
                .tool_setups
                .get(tool_id.as_str())
                .and_then(|setup| setup.default_config_dir.as_ref())
                .map(|dir| canonical_path(dir));
            if configured.as_deref() == Some(account_dir.as_path()) {
                anyhow::bail!(
                    "This account's folder is set as the tool's default config folder — change it in Settings before deleting"
                );
            }
        }

        // Clear the pointer BEFORE removing the files, so a failure here aborts the delete instead
        // of leaving the plain command aimed at a deleted profile.
        if was_active && matches!(tool_id, ToolId::Claude | ToolId::Codex) {
            clear_active_profile(&tool_id, &self.store)
                .context("Couldn't clear the account in use — nothing was deleted")?;
            install_shell_hook(&self.store)
                .context("Couldn't update the shell hook — nothing was deleted")?;
        }

        // Best-effort: resolve the org this account is logged into before its credentials go
        // away, so the Usage tab can still attribute its past sessions by name. An HTTP failure
        // or a missing token must never block the delete.
        if let Some((name, config_dir)) = pending_org_lookup {
            if let Ok(identity) = read_claude_profile(&config_dir) {
                if let Ok(mut data) = self.data.lock() {
                    if upsert_claude_org(&mut data.claude_orgs, &identity, &account_id, &name, &now()) {
                        let _ = self.store.save(&data);
                    }
                }
            }
        }

        delete_account_files(&tool_id, &self.store, &account_id)?;
        if let Some(name) = launcher {
            let _ = remove_launcher(&name);
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

/// Total size of a directory tree in bytes (best-effort; unreadable entries are skipped). Follows
/// real files only — symlinked shared stores (e.g. `projects` → `~/.claude/projects`) are not
/// followed, so an orphan dir's size reflects only its own on-disk data, not the shared store.
fn dir_size_bytes(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.path().symlink_metadata() else {
            continue;
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            continue; // don't follow symlinks (shared stores) or count their target
        } else if file_type.is_dir() {
            total += dir_size_bytes(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

/// Whether a profile dir looks like it's being used by a live CLI session right now: any real file
/// under it modified within the last few minutes. Used only to warn before deletion — never to block.
fn dir_recently_active(path: &std::path::Path) -> bool {
    const RECENT_SECS: u64 = 10 * 60;
    fn newest_mtime_within(path: &std::path::Path, cutoff: std::time::SystemTime) -> bool {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.file_type().is_dir() {
                if newest_mtime_within(&entry.path(), cutoff) {
                    return true;
                }
            } else if meta.modified().is_ok_and(|m| m >= cutoff) {
                return true;
            }
        }
        false
    }
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(RECENT_SECS);
    newest_mtime_within(path, cutoff)
}

/// Format a byte count as a short human-readable size (e.g. "4.0 KB", "12.3 MB").
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
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
    // Every profile-based CLI gets a "Machine default" row: the login the tool already has
    // outside the app. Antigravity is excluded (it captures/swaps instead).
    for tool_id in [
        ToolId::Claude,
        ToolId::Codex,
        ToolId::Cursor,
        ToolId::Opencode,
    ] {
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
            hidden: false,
            weekly_lock: None,
            avatar_url: None,
            api_provider: None,
        });
    }
}

fn migrate_auto_switch_settings(data: &mut StoredState) {
    let legacy = default_auto_switch_setting_from_legacy(data);
    // Auto-switch stays on the two CLIs whose quota the app can act on mid-session.
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
    let tools = [
        ToolId::Claude,
        ToolId::Codex,
        ToolId::Cursor,
        ToolId::Opencode,
        ToolId::Antigravity,
    ]
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
        .filter(|account| !account.hidden)
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
    for tool_id in [
        ToolId::Claude,
        ToolId::Codex,
        ToolId::Cursor,
        ToolId::Opencode,
    ] {
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
/// Keep an overlay opacity usable: never fully invisible (the user could not find it again) and
/// never above solid. A non-finite value from a hand-edited state file falls back to `fallback`.
fn clamp_opacity(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.15, 1.0)
    } else {
        fallback
    }
}

/// Resolve symlinks/`..` so two paths that name the same folder compare equal. Falls back to the
/// path as given when it doesn't exist yet (canonicalize fails on missing paths).
fn canonical_path(path: &std::path::Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn active_account_id_for(store: &Store, tool_id: &ToolId, accounts: &[Account]) -> Option<String> {
    if matches!(tool_id, ToolId::Antigravity) {
        // The account in use = the account whose token matches the IDE's current token in state.vscdb.
        let current = antigravity_current_token()?;
        return accounts
            .iter()
            .find(|account| {
                !account.hidden
                    && antigravity_saved_token(store, &account.id).as_deref() == Some(current.as_str())
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
                !account.hidden
                    && !account.is_default
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
                && !account.hidden
                && !account.is_locked()
                && Some(account.id.as_str()) != excluded_account_id
                && !matches!(account.state, AccountState::NeedsLogin)
                && max_percent_used(account) < threshold
                // Skip accounts with no quota data yet (unsure whether any is left).
                && account.quota.as_ref().is_some_and(|q| q.error.is_none())
        })
        .min_by(|left, right| max_percent_used(left).total_cmp(&max_percent_used(right)))
}

/// Tools whose quota has a weekly window to lock on.
fn supports_weekly_lock(tool_id: &ToolId) -> bool {
    matches!(tool_id, ToolId::Claude | ToolId::Codex | ToolId::Opencode)
}

/// What the weekly reserve lock should do after a fresh quota read: `Some(true)` lock,
/// `Some(false)` unlock (the weekly window reset below the threshold), `None` leave as is.
/// A failed or partial quota read never changes the lock.
fn weekly_lock_transition(account: &Account) -> Option<bool> {
    let lock = account.weekly_lock.as_ref().filter(|lock| lock.enabled)?;
    let quota = account.quota.as_ref().filter(|quota| quota.error.is_none())?;
    let used = quota.weekly.percent_used?;
    match (lock.locked, used >= lock.threshold) {
        (false, true) => Some(true),
        (true, false) => Some(false),
        _ => None,
    }
}

/// Recreate the per-account launcher after unhiding. Missing binary path is a no-op — the
/// account is visible again and the user can set the command from the card.
fn restore_launcher_file(account: &Account, store: &Store, data: &StoredState) -> Result<()> {
    let Some(name) = account.launcher_command.as_deref() else {
        return Ok(());
    };
    if matches!(account.tool_id, ToolId::Antigravity) {
        return Ok(());
    }
    let Some(binary) = configured_binary_path(data, &account.tool_id) else {
        return Ok(());
    };
    if let Some(api) = &account.api_provider {
        write_api_launcher(
            &account.tool_id,
            store,
            &account.id,
            name,
            &api.model,
            api.bypass,
            &binary,
        )
    } else {
        write_launcher(&account.tool_id, store, &account.id, name, &binary)
    }
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
        ToolId::Cursor => "cursor-api",
        ToolId::Opencode => "opencode-api",
        ToolId::Antigravity => "antigravity-api",
    }
}

/// Record that `account_id` is logged into the org `identity` describes. An account maps to at
/// most one org, so it's first removed from any OTHER record (a re-login into another
/// subscription); `account_ids`/`account_names` stay aligned by position. Missing email/org-name
/// in a fresh profile never erases values already stored. Returns whether the registry changed.
fn upsert_claude_org(
    registry: &mut std::collections::BTreeMap<String, ClaudeOrgRecord>,
    identity: &ClaudeProfileIdentity,
    account_id: &str,
    account_name: &str,
    now: &str,
) -> bool {
    let mut changed = false;
    for (org_uuid, record) in registry.iter_mut() {
        if org_uuid == &identity.org_uuid {
            continue;
        }
        if let Some(index) = record.account_ids.iter().position(|id| id == account_id) {
            record.account_ids.remove(index);
            if index < record.account_names.len() {
                record.account_names.remove(index);
            }
            changed = true;
        }
    }
    let record = registry.entry(identity.org_uuid.clone()).or_default();
    // A hand-edited/older record can have fewer names than ids — pad so index writes below hold.
    if record.account_names.len() < record.account_ids.len() {
        record
            .account_names
            .resize(record.account_ids.len(), String::new());
        changed = true;
    }
    if identity.email.is_some() && record.email != identity.email {
        record.email = identity.email.clone();
        changed = true;
    }
    if identity.org_name.is_some() && record.organization_name != identity.org_name {
        record.organization_name = identity.org_name.clone();
        changed = true;
    }
    match record.account_ids.iter().position(|id| id == account_id) {
        Some(index) => {
            if record.account_names[index] != account_name {
                record.account_names[index] = account_name.to_string();
                changed = true;
            }
        }
        None => {
            record.account_ids.push(account_id.to_string());
            record.account_names.push(account_name.to_string());
            changed = true;
        }
    }
    if record.last_seen != now {
        record.last_seen = now.to_string();
        changed = true;
    }
    changed
}

/// How `usage::build_report` should label each known org. `label` prefers the login email, then
/// the organization name, then a short uuid; `account_names` uses each account's CURRENT name
/// (renames don't rewrite the registry eagerly), falling back to the stored name once the account
/// is gone. `removed` = none of the record's ids is an eligible current Claude account.
fn claude_org_labels(
    registry: &std::collections::BTreeMap<String, ClaudeOrgRecord>,
    accounts: &[Account],
) -> std::collections::BTreeMap<String, UsageOrgLabel> {
    registry
        .iter()
        .map(|(org_uuid, record)| {
            let label = record
                .email
                .clone()
                .or_else(|| record.organization_name.clone())
                .unwrap_or_else(|| {
                    format!("Org {}", org_uuid.chars().take(8).collect::<String>())
                });
            let account_names = record
                .account_ids
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    accounts
                        .iter()
                        .find(|account| &account.id == id)
                        .map(|account| account.name.clone())
                        .or_else(|| record.account_names.get(index).cloned())
                        .unwrap_or_default()
                })
                .collect();
            let removed = !record.account_ids.iter().any(|id| {
                accounts.iter().any(|account| {
                    &account.id == id
                        && account.tool_id == ToolId::Claude
                        && account.api_provider.is_none()
                        && !is_virtual_api_account(account)
                })
            });
            (
                org_uuid.clone(),
                UsageOrgLabel {
                    label,
                    email: record.email.clone(),
                    account_names,
                    removed,
                },
            )
        })
        .collect()
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

/// A rate-limited read (HTTP 429) carries no numbers. Keep the previous read's windows, and its
/// `updated_at` so the UI can tell how old they are, rather than blanking the account for the whole
/// Retry-After. `error` stays set, so auto-switch/exhaustion/prime logic still treats the numbers as
/// unknown. A window whose reset has already passed is dropped: its percentage no longer applies.
fn keep_last_quota_when_rate_limited(
    previous: Option<&QuotaInfo>,
    mut next: QuotaInfo,
) -> QuotaInfo {
    let Some(previous) = previous else {
        return next;
    };
    let has_numbers =
        previous.five_hour.percent_used.is_some() || previous.weekly.percent_used.is_some();
    if next.rate_limited_until.is_none() || !has_numbers {
        return next;
    }
    let now = chrono::Utc::now();
    let still_valid = |window: &crate::models::QuotaWindow| {
        let expired = window
            .reset_at
            .as_deref()
            .and_then(|reset| chrono::DateTime::parse_from_rfc3339(reset).ok())
            .is_some_and(|reset| reset < now);
        if expired {
            crate::models::QuotaWindow {
                label: window.label.clone(),
                ..Default::default()
            }
        } else {
            window.clone()
        }
    };
    next.five_hour = still_valid(&previous.five_hour);
    next.weekly = still_valid(&previous.weekly);
    next.updated_at = previous.updated_at.clone();
    next
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
        // Show a readable local time with the UTC-offset label, not the raw ISO/UTC string.
        .map(|iso| local_time_label_from_iso(&iso))
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

fn notify_weekly_lock(app: &AppHandle, account: &Account, locked: bool) {
    let (title, body) = if locked {
        let used = account
            .quota
            .as_ref()
            .and_then(|quota| quota.weekly.percent_used)
            .map(|percent| format!("{}%", percent.round() as i64))
            .unwrap_or_else(|| "the limit".to_string());
        (
            "Account locked",
            format!(
                "{} reached {used} of its weekly quota — locked to save the rest. Unlock it on its card to keep using it.",
                account.name
            ),
        )
    } else {
        (
            "Account unlocked",
            format!("{}'s weekly quota reset — unlocked and ready to use.", account.name),
        )
    };
    let _ = app.notification().builder().title(title).body(&body).show();
    let _ = app.emit("auto-switched", body);
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// One "Prime ngay" job: the account identity + resolved profile dir the attempt runs against.
struct PrimeJob {
    tool_id: ToolId,
    account_id: String,
    account_name: String,
    config_dir: std::path::PathBuf,
}

/// Local timestamp for log lines: `YYYY-MM-DD HH:MM:SS` (machine timezone).
fn local_log_timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// RAII reset of the `priming` overlap flag. Holds a clone of the `Arc<AtomicBool>`, so it is plainly
/// `Send` and can move into a backgrounded manual-prime worker to hold the flag for the whole
/// send+confirm and release it (exactly once) when the worker finishes — or on any early return.
struct PrimingGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl PrimingGuard {
    fn new(state: &ManagedState) -> Self {
        PrimingGuard(std::sync::Arc::clone(&state.priming))
    }
}

impl Drop for PrimingGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Render an ISO 8601 instant as local `HH:MM` for log lines. Falls back to the raw string.
fn local_hhmm_from_iso(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

/// Render an ISO 8601 instant as a human-friendly LOCAL time with an explicit UTC-offset label,
/// e.g. `16:32 (UTC+7)`. For user-facing notifications — a raw `…T11:20:00+00:00` reads as "11:20"
/// and gets mistaken for a local morning time when it's actually 18:20 in a +7 zone. Falls back to
/// the raw string if unparseable.
fn local_time_label_from_iso(iso: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(iso) {
        Ok(t) => {
            let local = t.with_timezone(&chrono::Local);
            // Offset in whole hours (e.g. +7); show minutes too only when the zone isn't on the hour.
            let secs = local.offset().local_minus_utc();
            let sign = if secs < 0 { '-' } else { '+' };
            let abs = secs.abs();
            let (h, m) = (abs / 3600, (abs % 3600) / 60);
            let off = if m == 0 {
                format!("UTC{sign}{h}")
            } else {
                format!("UTC{sign}{h}:{m:02}")
            };
            format!("{} ({off})", local.format("%H:%M"))
        }
        Err(_) => iso.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(percent: Option<f64>, reset_at: Option<String>) -> crate::models::QuotaWindow {
        crate::models::QuotaWindow {
            label: "w".to_string(),
            percent_used: percent,
            reset_at,
            is_active: None,
        }
    }

    #[test]
    fn rate_limited_read_keeps_last_numbers_but_drops_expired_windows() {
        let future = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
        let past = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        let mut previous = QuotaInfo::with_message("x");
        previous.error = None;
        previous.five_hour = window(Some(79.0), Some(past));
        previous.weekly = window(Some(65.0), Some(future.clone()));
        previous.updated_at = Some("2026-10-03T18:02:00Z".to_string());

        let mut limited = QuotaInfo::with_message("Anthropic giới hạn … (HTTP 429)");
        limited.rate_limited_until = Some(future);
        let kept = keep_last_quota_when_rate_limited(Some(&previous), limited);
        assert_eq!(kept.five_hour.percent_used, None, "5h window already reset");
        assert_eq!(kept.weekly.percent_used, Some(65.0));
        assert_eq!(kept.updated_at.as_deref(), Some("2026-10-03T18:02:00Z"));
        assert!(kept.error.is_some(), "error kept for auto-switch/prime");

        // Any other error replaces the numbers as before.
        let other = keep_last_quota_when_rate_limited(
            Some(&previous),
            QuotaInfo::with_message("Couldn't read quota: HTTP 401"),
        );
        assert_eq!(other.weekly.percent_used, None);
    }

    #[test]
    fn local_time_label_shows_local_hhmm_with_offset() {
        // A UTC instant renders as the LOCAL time plus a "(UTC±N)" label — never the raw UTC HH:MM,
        // which is what made "11:20+00:00" read as a morning time when it was 18:20 in a +7 zone.
        let got = local_time_label_from_iso("2026-06-18T11:20:00+00:00");
        let expected_local = chrono::DateTime::parse_from_rfc3339("2026-06-18T11:20:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Local)
            .format("%H:%M")
            .to_string();
        assert!(
            got.starts_with(&expected_local),
            "expected label to start with local time {expected_local}, got {got}"
        );
        assert!(
            got.contains("(UTC"),
            "expected a UTC-offset label, got {got}"
        );
    }

    #[test]
    fn local_time_label_passes_through_unparseable() {
        assert_eq!(local_time_label_from_iso("nope"), "nope");
    }

    fn test_account(id: &str, hidden: bool, percent: f64) -> Account {
        Account {
            id: id.to_string(),
            tool_id: ToolId::Claude,
            name: id.to_string(),
            state: AccountState::Idle,
            fingerprint: format!("profile:{id}"),
            created_at: "2026-09-14T00:00:00Z".to_string(),
            updated_at: "2026-09-14T00:00:00Z".to_string(),
            last_used_at: None,
            quota: Some(QuotaInfo {
                five_hour: crate::models::QuotaWindow {
                    label: "5-hour".into(),
                    percent_used: Some(percent),
                    reset_at: None,
                    is_active: None,
                },
                weekly: crate::models::QuotaWindow {
                    label: "weekly".into(),
                    percent_used: Some(percent),
                    reset_at: None,
                    is_active: None,
                },
                models: None,
                plan: None,
                rate_limit_reset_credits: None,
                prime_available: None,
                rate_limited_until: None,
                updated_at: None,
                error: None,
            }),
            launcher_command: None,
            is_default: false,
            hidden,
            weekly_lock: None,
            avatar_url: None,
            api_provider: None,
        }
    }

    fn with_weekly(mut account: Account, weekly: Option<f64>, lock: WeeklyLock) -> Account {
        if let Some(quota) = account.quota.as_mut() {
            quota.weekly.percent_used = weekly;
        }
        account.weekly_lock = Some(lock);
        account
    }

    fn lock(enabled: bool, threshold: f64, locked: bool) -> WeeklyLock {
        WeeklyLock { enabled, threshold, locked }
    }

    #[test]
    fn weekly_lock_locks_at_threshold() {
        let account = with_weekly(test_account("a", false, 10.0), Some(80.0), lock(true, 80.0, false));
        assert_eq!(weekly_lock_transition(&account), Some(true));
    }

    #[test]
    fn weekly_lock_stays_below_threshold() {
        let account = with_weekly(test_account("a", false, 10.0), Some(79.0), lock(true, 80.0, false));
        assert_eq!(weekly_lock_transition(&account), None);
    }

    #[test]
    fn weekly_lock_unlocks_after_weekly_reset() {
        let account = with_weekly(test_account("a", false, 10.0), Some(2.0), lock(true, 80.0, true));
        assert_eq!(weekly_lock_transition(&account), Some(false));
    }

    #[test]
    fn weekly_lock_ignores_disarmed_and_unknown_quota() {
        let off = with_weekly(test_account("a", false, 10.0), Some(95.0), lock(false, 80.0, false));
        assert_eq!(weekly_lock_transition(&off), None);
        let unknown = with_weekly(test_account("b", false, 10.0), None, lock(true, 80.0, true));
        assert_eq!(weekly_lock_transition(&unknown), None);
        let mut failed = with_weekly(test_account("c", false, 10.0), Some(0.0), lock(true, 80.0, true));
        failed.quota.as_mut().unwrap().error = Some("network".into());
        assert_eq!(weekly_lock_transition(&failed), None);
    }

    #[test]
    fn best_replacement_skips_locked_accounts() {
        let accounts = vec![
            with_weekly(test_account("locked-plenty", false, 0.0), Some(0.0), lock(true, 80.0, true)),
            test_account("open-used", false, 40.0),
        ];
        let best = best_replacement(&accounts, &ToolId::Claude, 100.0, None).unwrap();
        assert_eq!(best.id, "open-used");
    }

    #[test]
    fn best_replacement_skips_hidden_accounts() {
        let accounts = vec![
            test_account("hidden-plenty", true, 0.0),
            test_account("visible-used", false, 40.0),
        ];
        let best = best_replacement(&accounts, &ToolId::Claude, 100.0, None).unwrap();
        assert_eq!(best.id, "visible-used");
    }

    fn claude_identity(org_uuid: &str, org_name: Option<&str>, email: Option<&str>) -> ClaudeProfileIdentity {
        ClaudeProfileIdentity {
            org_uuid: org_uuid.to_string(),
            org_name: org_name.map(str::to_string),
            email: email.map(str::to_string),
        }
    }

    #[test]
    fn upsert_claude_org_adds_account_and_is_idempotent() {
        let mut registry = std::collections::BTreeMap::new();
        let identity = claude_identity("org-1", Some("Acme"), Some("a@x.com"));
        assert!(upsert_claude_org(&mut registry, &identity, "acc1", "Work", "t1"));
        let record = &registry["org-1"];
        assert_eq!(record.account_ids, vec!["acc1".to_string()]);
        assert_eq!(record.account_names, vec!["Work".to_string()]);
        assert_eq!(record.email.as_deref(), Some("a@x.com"));
        assert_eq!(record.organization_name.as_deref(), Some("Acme"));
        assert_eq!(record.last_seen, "t1");
        // Same data at the same timestamp → nothing to change.
        assert!(!upsert_claude_org(&mut registry, &identity, "acc1", "Work", "t1"));
        // A fresh lookup only bumps last_seen.
        assert!(upsert_claude_org(&mut registry, &identity, "acc1", "Work", "t2"));
        assert_eq!(registry["org-1"].last_seen, "t2");
        // A partial profile response must not erase the stored email/name.
        let partial = claude_identity("org-1", None, None);
        upsert_claude_org(&mut registry, &partial, "acc1", "Work", "t3");
        assert_eq!(registry["org-1"].email.as_deref(), Some("a@x.com"));
        assert_eq!(registry["org-1"].organization_name.as_deref(), Some("Acme"));
    }

    #[test]
    fn upsert_claude_org_refreshes_renamed_account() {
        let mut registry = std::collections::BTreeMap::new();
        let identity = claude_identity("org-1", None, Some("a@x.com"));
        upsert_claude_org(&mut registry, &identity, "acc1", "Old", "t1");
        assert!(upsert_claude_org(&mut registry, &identity, "acc1", "New", "t2"));
        assert_eq!(registry["org-1"].account_ids, vec!["acc1".to_string()]);
        assert_eq!(registry["org-1"].account_names, vec!["New".to_string()]);
    }

    #[test]
    fn upsert_claude_org_moves_account_between_orgs() {
        let mut registry = std::collections::BTreeMap::new();
        let org_a = claude_identity("org-a", None, Some("a@x.com"));
        let org_b = claude_identity("org-b", None, Some("b@x.com"));
        upsert_claude_org(&mut registry, &org_a, "acc1", "Work", "t1");
        upsert_claude_org(&mut registry, &org_b, "acc2", "Other", "t1");
        // acc1 re-logs into org-b: it must leave org-a entirely (one org per account) and join
        // org-b keeping account_ids/account_names aligned by position.
        assert!(upsert_claude_org(&mut registry, &org_b, "acc1", "Work", "t2"));
        assert!(registry["org-a"].account_ids.is_empty());
        assert!(registry["org-a"].account_names.is_empty());
        assert_eq!(
            registry["org-b"].account_ids,
            vec!["acc2".to_string(), "acc1".to_string()]
        );
        assert_eq!(
            registry["org-b"].account_names,
            vec!["Other".to_string(), "Work".to_string()]
        );
    }

    fn org_record(
        email: Option<&str>,
        org_name: Option<&str>,
        ids: &[&str],
        names: &[&str],
    ) -> ClaudeOrgRecord {
        ClaudeOrgRecord {
            email: email.map(str::to_string),
            organization_name: org_name.map(str::to_string),
            account_ids: ids.iter().map(|id| id.to_string()).collect(),
            account_names: names.iter().map(|name| name.to_string()).collect(),
            last_seen: String::new(),
        }
    }

    #[test]
    fn claude_org_labels_prefers_email_then_org_name_then_uuid() {
        let mut registry = std::collections::BTreeMap::new();
        registry.insert(
            "uuid-with-email".to_string(),
            org_record(Some("me@x.com"), Some("Org Name"), &[], &[]),
        );
        registry.insert(
            "uuid-with-org".to_string(),
            org_record(None, Some("Org Name"), &[], &[]),
        );
        registry.insert("uuid-only".to_string(), org_record(None, None, &[], &[]));
        let labels = claude_org_labels(&registry, &[]);
        assert_eq!(labels["uuid-with-email"].label, "me@x.com");
        assert_eq!(labels["uuid-with-org"].label, "Org Name");
        assert_eq!(labels["uuid-only"].label, "Org uuid-onl");
        // No current accounts at all → every org counts as removed.
        assert!(labels.values().all(|label| label.removed));
    }

    #[test]
    fn claude_org_labels_marks_removed_and_prefers_current_names() {
        // "keep" is a normal Claude OAuth account; "api-acct" is a local-API account (not a
        // subscription login) — it must not count as a current eligible account.
        let current = test_account("keep", false, 0.0);
        let mut api = test_account("api-acct", false, 0.0);
        api.fingerprint = "api-local".to_string();
        let accounts = vec![current, api];

        let mut registry = std::collections::BTreeMap::new();
        registry.insert(
            "org-live".to_string(),
            org_record(Some("live@x.com"), None, &["keep"], &["Stale Name"]),
        );
        registry.insert(
            "org-gone".to_string(),
            org_record(Some("gone@x.com"), None, &["deleted"], &["Deleted Acct"]),
        );
        registry.insert(
            "org-api".to_string(),
            org_record(None, None, &["api-acct"], &["API"]),
        );
        let labels = claude_org_labels(&registry, &accounts);

        let live = &labels["org-live"];
        assert!(!live.removed);
        // The live account's current name wins over the stale stored one.
        assert_eq!(live.account_names, vec!["keep".to_string()]);

        let gone = &labels["org-gone"];
        assert!(gone.removed);
        // A removed account keeps the name recorded when it was last seen.
        assert_eq!(gone.account_names, vec!["Deleted Acct".to_string()]);

        // api-acct still exists but is not a Claude OAuth account → the org is removed.
        assert!(labels["org-api"].removed);
    }
}

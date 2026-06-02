use crate::models::{
    Account, AccountState, AddAccountInput, AppSnapshot, RenameAccountInput, SetLauncherInput,
    SwitchAccountInput, ToolId, ToolStatus, UsageReport,
};
use crate::quota::read_quota;
use crate::store::{normalize_account_states, Store, StoredState};
use crate::tools::{
    antigravity_capture, antigravity_current_token, antigravity_new_login, antigravity_open_ide,
    antigravity_quit_ide, antigravity_restore, antigravity_saved_profile, antigravity_saved_token,
    clear_active_profile,
    default_config_dir, delete_account_files, full_launcher_name, install_shell_hook, is_installed,
    launch_profile_login, launcher_name_collides_with_system, remove_launcher, write_active_profile,
    write_launcher,
};
use anyhow::{Context, Result};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

pub struct ManagedState {
    pub store: Store,
    pub data: Mutex<StoredState>,
}

impl ManagedState {
    pub fn new() -> Result<Self> {
        let store = Store::new()?;
        let mut data = store.load()?;
        migrate_defaults(&mut data.accounts);
        store.save(&data)?;
        let managed = Self {
            store,
            data: Mutex::new(data),
        };
        // Clean up orphan active files: pointing to a deleted profile → clear + reinstall the hook.
        managed.heal_active_profiles();
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
            for dir in &valid_dirs {
                crate::tools::seed_onboarding(&tool_id, dir);
                crate::tools::link_shared_sessions(&tool_id, dir);
            }

            // Clear active if it points to a profile that belongs to no account.
            let active = self.store.active_profile_path(&tool_id);
            if let Ok(target) = std::fs::read_to_string(&active) {
                let target = target.trim();
                if !target.is_empty()
                    && !valid_dirs.iter().any(|d| d.to_string_lossy() == target)
                {
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
        Ok(build_snapshot(&self.store, &data))
    }

    /// Build the token-usage report (Usage tab): incrementally scan every Claude/Codex config
    /// dir on the machine, aggregate per tool, and price it via the LiteLLM cache. Antigravity
    /// is excluded (no token logs). Cheap to call repeatedly thanks to the per-file cursor cache.
    pub fn usage_report(&self) -> UsageReport {
        crate::usage::build_report(
            &self.store.usage_cache_path(),
            &self.store.price_cache_path(),
            &self.config_dirs(&ToolId::Claude),
            &self.config_dirs(&ToolId::Codex),
        )
    }

    /// Every config dir to scan for a tool: the machine default (`~/.claude`, `~/.codex`) plus
    /// every per-account profile dir under the app's accounts root.
    fn config_dirs(&self, tool_id: &ToolId) -> Vec<std::path::PathBuf> {
        let mut dirs = vec![default_config_dir(tool_id)];
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
        let (auto_switch, threshold) = {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            for account in data.accounts.iter_mut().filter(|account| {
                account.tool_id == tool_id && account.state != AccountState::NeedsLogin
            }) {
                let config_dir = account_config_dir(&self.store, account);
                account.quota = Some(read_quota(&account.tool_id, &config_dir));
                account.updated_at = now();
                account.state = if is_exhausted(account) {
                    AccountState::Exhausted
                } else if account.state == AccountState::Exhausted {
                    // quota recovered (below 100%) → drop the exhausted state.
                    AccountState::Idle
                } else {
                    account.state.clone()
                };
                if account.state == AccountState::Exhausted {
                    if let Some(app) = app {
                        notify_exhausted(app, account);
                    }
                }
            }
            self.store.save(&data)?;
            (data.auto_switch, data.auto_switch_threshold)
        };

        if auto_switch {
            self.maybe_auto_switch(&tool_id, threshold, app)?;
        }
        self.snapshot()
    }

    /// Enable/disable auto-switch + set the threshold (% used that triggers it).
    pub fn set_auto_switch(&self, enabled: bool, threshold: f64) -> Result<AppSnapshot> {
        {
            let mut data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.auto_switch = enabled;
            data.auto_switch_threshold = threshold.clamp(50.0, 100.0);
            self.store.save(&data)?;
        }
        self.snapshot()
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
        crate::tools::link_shared_sessions(tool_id, &config_dir);
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
                match (&new_profile, antigravity_saved_profile(&self.store, &account.id)) {
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
        let quota = read_quota(&ToolId::Antigravity, &default_config_dir(&ToolId::Antigravity));
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
            });
            self.store.save(&data)?;
        }
        self.snapshot()
    }

    fn create_profile_account(&self, app: &AppHandle, input: AddAccountInput) -> Result<AppSnapshot> {
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

        launch_profile_login(&input.tool_id, &self.store, &id)
            .context("Login not completed, account not added")?;
        write_launcher(&input.tool_id, &self.store, &id, &full_launcher)
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
            });
            self.store.save(&data)?;
        }

        // Background poll: login done (token appears) → NeedsLogin to Idle + read quota.
        spawn_login_watch(app.clone(), input.tool_id.clone(), id);
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
        let old_launcher = {
            let data = self
                .data
                .lock()
                .map_err(|_| anyhow::anyhow!("state lock poisoned"))?;
            data.accounts
                .iter()
                .find(|a| a.tool_id == input.tool_id && a.id == input.account_id)
                .context("Account not found")?
                .launcher_command
                .clone()
        };

        write_launcher(&input.tool_id, &self.store, &input.account_id, &full)
            .context("Couldn't create the custom command")?;
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
            for account in data
                .accounts
                .iter_mut()
                .filter(|account| account.tool_id == input.tool_id && account.id == input.account_id)
            {
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
    fn validated_launcher(
        &self,
        tool_id: &ToolId,
        account_id: &str,
        raw: &str,
    ) -> Result<String> {
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
fn account_config_dir(store: &Store, account: &Account) -> std::path::PathBuf {
    if account.fingerprint.starts_with("profile:") {
        store.account_dir(&account.tool_id, &account.id)
    } else {
        default_config_dir(&account.tool_id)
    }
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
        });
    }
}

fn build_snapshot(store: &Store, data: &StoredState) -> AppSnapshot {
    let tools = [ToolId::Claude, ToolId::Codex, ToolId::Antigravity]
        .into_iter()
        .map(|tool_id| {
            let mut accounts = data
                .accounts
                .iter()
                .filter(|account| account.tool_id == tool_id)
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
                installed: is_installed(&tool_id),
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
    }
}

/// The account the PLAIN COMMAND is using. For Claude/Codex this is the real source of truth:
/// the active file (`active/<tool>.profile`) that the shell hook reads to export the config dir —
/// NOT inferred from `state==Active` (an exhausted account is still the one the plain command uses,
/// but its state is Exhausted, so inferring from state would be wrong). Empty/missing file = machine Default.
/// Antigravity is copy-swap (no active file), so it still follows `state==Active`.
fn active_account_id_for(
    store: &Store,
    tool_id: &ToolId,
    accounts: &[Account],
) -> Option<String> {
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

use crate::models::ToolId;
use crate::store::Store;
use anyhow::{Context, Result};
use keyring::Entry;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP_KEYCHAIN_SERVICE: &str = "AI Account Switcher";

/// The tool's default config dir (when the account isn't profile-mode, or is
/// the "Default" account pointing at the machine's original session).
pub fn default_config_dir(tool_id: &ToolId) -> PathBuf {
    let home = home_dir();
    match tool_id {
        ToolId::Claude => home.join(".claude"),
        ToolId::Codex => home.join(".codex"),
        // Antigravity IDE: the machine's default userData on macOS (original login).
        ToolId::Antigravity => home.join("Library/Application Support/Antigravity IDE"),
    }
}

/// Path to the Antigravity IDE .app (to open with `open`).
fn antigravity_ide_app() -> Option<PathBuf> {
    let app = PathBuf::from("/Applications/Antigravity IDE.app");
    if app.exists() {
        Some(app)
    } else {
        None
    }
}

/// Whether Antigravity IDE can be opened (.app installed or a shim in PATH).
fn antigravity_ide_available() -> bool {
    antigravity_ide_app().is_some()
        || home_dir()
            .join(".antigravity-ide/antigravity-ide/bin/antigravity-ide")
            .exists()
        || command_path("antigravity-ide").is_some()
}

// ---------------------------------------------------------------------------
// Antigravity IDE switch = copy-swap the identity token in the default state.vscdb.
//
// The Antigravity IDE login lives in key `antigravityUnifiedStateSync.oauthToken`
// (base64/protobuf plaintext) inside the default userData's state.vscdb. It CAN'T
// be isolated with --user-data-dir (the OAuth callback deep-link routes back to
// the default instance). So: sign in to each account one at a time in the IDE →
// Capture token; switch = quit IDE → write the chosen account's token → reopen IDE.
// ---------------------------------------------------------------------------

/// state.vscdb of the Antigravity IDE default userData.
fn antigravity_state_db() -> PathBuf {
    default_config_dir(&ToolId::Antigravity).join("User/globalStorage/state.vscdb")
}

/// Account identity keys to swap (token + avatar). Other preferences are left untouched.
const AG_TOKEN_KEY: &str = "antigravityUnifiedStateSync.oauthToken";
const AG_PROFILE_KEY: &str = "antigravity.profileUrl";

/// Read one state.vscdb key as a safe SQL literal (quote() handles both TEXT/BLOB).
/// None if the key is absent or empty.
fn ag_read_key_literal(db: &Path, key: &str) -> Option<String> {
    let out = Command::new("sqlite3")
        .arg(db)
        .arg(format!(
            "SELECT quote(value) FROM ItemTable WHERE key='{key}';"
        ))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() || value == "NULL" {
        None
    } else {
        Some(value)
    }
}

/// Write (INSERT OR REPLACE) one key into state.vscdb with an already-quoted SQL literal.
fn ag_write_key(db: &Path, key: &str, literal: &str) -> Result<()> {
    let status = Command::new("sqlite3")
        .arg(db)
        .arg(format!(
            "INSERT OR REPLACE INTO ItemTable(key,value) VALUES('{key}',{literal});"
        ))
        .status()?;
    if !status.success() {
        anyhow::bail!("sqlite3 failed to write state.vscdb");
    }
    Ok(())
}

/// Save the account currently signed in to the IDE: copy token (+ avatar) from the
/// default state.vscdb into the account dir. Returns a fingerprint (token hash), or
/// an error if not signed in.
pub fn antigravity_capture(store: &Store, account_id: &str) -> Result<String> {
    let db = antigravity_state_db();
    let token = ag_read_key_literal(&db, AG_TOKEN_KEY)
        .context("Antigravity IDE isn't signed in — sign into the account you want to save first, then click Save")?;
    let dir = store.account_dir(&ToolId::Antigravity, account_id);
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("oauthToken.sql"), &token)?;
    if let Some(profile) = ag_read_key_literal(&db, AG_PROFILE_KEY) {
        fs::write(dir.join("profileUrl.sql"), profile)?;
    }
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    Ok(fingerprint(hasher.finalize().as_slice()))
}

/// The account's saved token (literal) — used to detect the account in use.
pub fn antigravity_saved_token(store: &Store, account_id: &str) -> Option<String> {
    fs::read_to_string(
        store
            .account_dir(&ToolId::Antigravity, account_id)
            .join("oauthToken.sql"),
    )
    .ok()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty())
}

/// Saved avatar (profileUrl literal) — identifies the Google account, used for dedup:
/// the same account has the same avatar even if the token blob differs (captured twice at different times).
pub fn antigravity_saved_profile(store: &Store, account_id: &str) -> Option<String> {
    fs::read_to_string(
        store
            .account_dir(&ToolId::Antigravity, account_id)
            .join("profileUrl.sql"),
    )
    .ok()
    .map(|p| p.trim().to_string())
    .filter(|p| !p.is_empty())
}

/// The real avatar URL (with SQL quoting removed) of the Antigravity account — for display in the UI.
pub fn antigravity_avatar_url(store: &Store, account_id: &str) -> Option<String> {
    let literal = antigravity_saved_profile(store, account_id)?;
    let inner = literal
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))?;
    Some(inner.replace("''", "'"))
}

/// The identity token currently in the IDE (to know which account is active).
pub fn antigravity_current_token() -> Option<String> {
    ag_read_key_literal(&antigravity_state_db(), AG_TOKEN_KEY)
}

/// Switch the IDE's account: write the account's saved token into the default state.vscdb.
pub fn antigravity_restore(store: &Store, account_id: &str) -> Result<()> {
    let db = antigravity_state_db();
    if !db.exists() {
        anyhow::bail!("Antigravity IDE's state.vscdb not found");
    }
    let dir = store.account_dir(&ToolId::Antigravity, account_id);
    let token = fs::read_to_string(dir.join("oauthToken.sql"))
        .context("Account has no saved token — save it again while signed into this account")?;
    ag_write_key(&db, AG_TOKEN_KEY, token.trim())?;
    if let Ok(profile) = fs::read_to_string(dir.join("profileUrl.sql")) {
        let _ = ag_write_key(&db, AG_PROFILE_KEY, profile.trim());
    }
    Ok(())
}

/// Quit Antigravity IDE (gracefully) then wait for the process to end — to keep it
/// from overwriting state.vscdb after the swap. Returns once it has quit or the wait times out (~5s).
pub fn antigravity_quit_ide() {
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "Antigravity IDE" to quit"#)
        .status();
    for _ in 0..25 {
        let running = Command::new("pgrep")
            .arg("-f")
            .arg("Antigravity IDE.app/Contents/MacOS")
            .output()
            .map(|out| !String::from_utf8_lossy(&out.stdout).trim().is_empty())
            .unwrap_or(false);
        if !running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Reopen Antigravity IDE (default userData) after swapping the token.
pub fn antigravity_open_ide() -> Result<()> {
    let app = antigravity_ide_app().context("Antigravity IDE is not installed")?;
    Command::new("/usr/bin/open")
        .arg("-a")
        .arg(&app)
        .spawn()
        .context("Couldn't open Antigravity IDE")?;
    Ok(())
}

/// Delete one key from state.vscdb.
fn ag_delete_key(db: &Path, key: &str) -> Result<()> {
    let status = Command::new("sqlite3")
        .arg(db)
        .arg(format!("DELETE FROM ItemTable WHERE key='{key}';"))
        .status()?;
    if !status.success() {
        anyhow::bail!("sqlite3 failed to delete the key");
    }
    Ok(())
}

/// Send the IDE to its sign-in screen to sign in a NEW account (never signed in before):
/// quit IDE (flush) → if the currently signed-in account is NOT yet saved, refuse (to
/// avoid losing the session) → delete the identity token from state.vscdb (sign out) →
/// reopen IDE. `saved_tokens` are the literal tokens of saved accounts, used to check
/// whether the current session has been saved.
pub fn antigravity_new_login(saved_tokens: &[String]) -> Result<()> {
    antigravity_quit_ide();
    let db = antigravity_state_db();
    if !db.exists() {
        anyhow::bail!("Antigravity IDE's state.vscdb not found");
    }
    if let Some(current) = ag_read_key_literal(&db, AG_TOKEN_KEY) {
        if !saved_tokens.iter().any(|token| token == &current) {
            let _ = antigravity_open_ide();
            anyhow::bail!(
                "Save the signed-in account first (Save current account), then sign in the new account — to avoid losing the session"
            );
        }
    }
    let _ = ag_delete_key(&db, AG_TOKEN_KEY);
    let _ = ag_delete_key(&db, AG_PROFILE_KEY);
    antigravity_open_ide()
}

/// Whether the Antigravity IDE account is signed in: the userData's state.vscdb has a non-empty
/// OAuth token (`antigravityUnifiedStateSync.oauthToken`). Read via sqlite3 (available on macOS).
fn antigravity_logged_in(data_dir: &Path) -> bool {
    let db = data_dir.join("User/globalStorage/state.vscdb");
    if !db.exists() {
        return false;
    }
    Command::new("sqlite3")
        .arg(&db)
        .arg("SELECT length(value) FROM ItemTable WHERE key='antigravityUnifiedStateSync.oauthToken';")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse::<i64>().ok())
        .is_some_and(|len| len > 0)
}

pub fn is_installed(tool_id: &ToolId) -> bool {
    match tool_id {
        ToolId::Claude => command_path("claude").is_some(),
        ToolId::Codex => command_path("codex").is_some(),
        ToolId::Antigravity => antigravity_ide_available(),
    }
}

pub fn command_name(tool_id: &ToolId) -> &'static str {
    match tool_id {
        ToolId::Claude => "claude",
        ToolId::Codex => "codex",
        ToolId::Antigravity => "antigravity-ide",
    }
}

pub fn launch_login(tool_id: &ToolId) -> Result<()> {
    match tool_id {
        // Claude 2.x does NOT have `claude login` — it must be `claude auth login`.
        ToolId::Claude => run_login_command("claude", &["auth", "login"]),
        ToolId::Codex => run_login_command("codex", &["login"]),
        ToolId::Antigravity => {
            if Path::new("/Applications/Antigravity.app").exists() {
                Command::new("open")
                    .arg("-a")
                    .arg("Antigravity")
                    .status()
                    .context("open Antigravity failed")?;
            } else {
                Command::new("open")
                    .arg("-a")
                    .arg("Google Antigravity")
                    .status()
                    .context("open Google Antigravity failed")?;
            }
            Ok(())
        }
    }
}

pub fn create_profile_with_default(
    tool_id: &ToolId,
    store: &Store,
    account_id: &str,
    default_config_dir: &Path,
) -> Result<PathBuf> {
    let profile = store.account_dir(tool_id, account_id);
    fs::create_dir_all(&profile)?;
    link_shared_sessions_to(tool_id, &profile, default_config_dir);
    Ok(profile)
}

/// Chat-session dir/file names that should be shared across a tool's accounts.
/// The token (auth.json/keychain) is NOT among these, so quota stays per-account.
/// - Claude: per-project transcripts in `projects/` + prompt history `history.jsonl`.
/// - Codex: conversation rollouts in `sessions/` + prompt history `history.jsonl`.
fn shared_session_names(tool_id: &ToolId) -> &'static [&'static str] {
    match tool_id {
        ToolId::Claude => &["projects", "history.jsonl"],
        ToolId::Codex => &["sessions", "history.jsonl"],
        ToolId::Antigravity => &[],
    }
}

/// Config/preferences that should be shared by normal OAuth accounts of the same tool.
/// Credentials are deliberately excluded:
/// - Claude credentials live in macOS Keychain under the profile-dir hash.
/// - Codex credentials live in `auth.json` / `api_key`.
///
/// API/proxy accounts must NOT use this list because their `settings.json` / `config.toml`
/// contains account-specific gateway settings.
fn shared_config_names(tool_id: &ToolId) -> &'static [&'static str] {
    match tool_id {
        ToolId::Claude => &[
            ".claude.json",
            "settings.json",
            "settings.local.json",
            "plugins",
            "rules",
            "commands",
            "agents",
        ],
        // SQLite databases are intentionally excluded: sharing a live .sqlite (+ WAL/SHM) between
        // the default Codex process and a profile Codex process causes write-lock contention.
        // When both run simultaneously, the profile process blocks waiting for the WAL write lock
        // held by the default process, which causes the WebSocket initialisation to time out
        // ("Reconnecting... 4/5"). Each account keeps its own memories and goals DBs.
        ToolId::Codex => &["config.toml", "rules", "skills", "memories"],
        ToolId::Antigravity => &[],
    }
}

fn shared_target_to(tool_id: &ToolId, name: &str, default_config_dir: &Path) -> PathBuf {
    if matches!(tool_id, ToolId::Claude) && name == ".claude.json" {
        home_dir().join(".claude.json")
    } else {
        default_config_dir.join(name)
    }
}

/// Share session/history across accounts: symlink the profile's chat-session entries
/// to the original config dir (`~/.claude`, `~/.codex`). This lets any account resume
/// a session created by another account, even in the same project. The token stays
/// per-profile so quotas don't mix. Idempotent — safe to call again.
pub fn link_shared_sessions_to(tool_id: &ToolId, profile: &Path, default_config_dir: &Path) {
    for name in shared_session_names(tool_id) {
        link_shared_entry(tool_id, profile, name, true, default_config_dir);
    }
}

/// Share CLI config/memory across normal OAuth profiles. This keeps account switching focused on
/// credentials/quota while user settings, skills, rules, and Codex memory stay consistent.
pub fn link_shared_config_to(tool_id: &ToolId, profile: &Path, default_config_dir: &Path) {
    // Remove any stale symlinks that were previously created but are no longer in the shared list
    // (e.g. memories_1.sqlite was removed to fix SQLite lock contention).
    remove_stale_shared_symlinks(tool_id, profile, default_config_dir);
    for name in shared_config_names(tool_id) {
        link_shared_entry(tool_id, profile, name, false, default_config_dir);
    }
}

/// Remove symlinks in `profile` that point into `default_config_dir` but are no longer in the
/// shared-config list. Called before re-applying the list so old symlinks don't linger.
fn remove_stale_shared_symlinks(tool_id: &ToolId, profile: &Path, default_config_dir: &Path) {
    let still_shared: std::collections::HashSet<&str> =
        shared_config_names(tool_id).iter().copied().collect();
    // Previously-shared Codex SQLite files that were removed from the list.
    let formerly_shared: &[&str] = match tool_id {
        ToolId::Codex => &[
            "memories_1.sqlite",
            "memories_1.sqlite-wal",
            "memories_1.sqlite-shm",
            "goals_1.sqlite",
            "goals_1.sqlite-wal",
            "goals_1.sqlite-shm",
        ],
        _ => &[],
    };
    for name in formerly_shared {
        if still_shared.contains(name) {
            continue;
        }
        let link = profile.join(name);
        let target = shared_target_to(tool_id, name, default_config_dir);
        if fs::read_link(&link).is_ok_and(|t| t == target) {
            let _ = fs::remove_file(&link);
        }
    }
}

fn link_shared_entry(
    tool_id: &ToolId,
    profile: &Path,
    name: &str,
    merge_dirs: bool,
    default_config_dir: &Path,
) {
    // Don't link to itself (Default account = ~/.claude, ~/.codex).
    if profile == default_config_dir {
        return;
    }

    let target = shared_target_to(tool_id, name, default_config_dir);
    let link = profile.join(name);
    if link == target || fs::read_link(&link).is_ok_and(|t| t == target) {
        return;
    }

    let is_real_dir =
        link.is_dir() && fs::symlink_metadata(&link).is_ok_and(|m| !m.file_type().is_symlink());
    if is_real_dir {
        if target.exists() || merge_dirs {
            // Rename the real dir aside FIRST, then merge from the stable snapshot. The rename is
            // atomic, so a CLI writing into `link` concurrently either landed before the move (its
            // data is in the snapshot) or lands after the symlink is created (it follows the symlink
            // into the shared store). Without this, a write between "copy" and "remove" — or a file
            // that collided and was skipped during copy — would be destroyed by `remove_dir_all`.
            // Append a suffix to the FULL name (not `with_extension`, which would mangle a dotted
            // name like `foo.bar` into `foo.migrating`).
            let stash = {
                let mut name = link.file_name().unwrap_or_default().to_os_string();
                name.push(".migrating");
                link.with_file_name(name)
            };
            let _ = fs::remove_dir_all(&stash); // clear a leftover stash from an interrupted run
            if fs::rename(&link, &stash).is_ok() {
                merge_dir_into(&stash, &target);
                // Only discard the stash once everything in it also exists in the shared store, so a
                // collision-skipped file is never silently lost; otherwise keep it for recovery.
                if dir_contents_all_present_in(&stash, &target) {
                    let _ = fs::remove_dir_all(&stash);
                }
            }
        } else if fs::rename(&link, &target).is_err() {
            // Target's parent may be missing on a first move; create it and retry, else fall back to
            // a copy-merge so we still establish the symlink rather than abandoning the dir.
            if let Some(parent) = target.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::rename(&link, &target).is_err() {
                merge_dir_into(&link, &target);
                let _ = fs::remove_dir_all(&link);
            }
        }
    } else if link.is_file()
        && fs::symlink_metadata(&link).is_ok_and(|m| !m.file_type().is_symlink())
    {
        if !target.exists() {
            if let Some(parent) = target.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::rename(&link, &target);
        }
        let _ = fs::remove_file(&link);
    } else if link.exists() || fs::symlink_metadata(&link).is_ok() {
        let _ = fs::remove_file(&link);
    }

    if !target.exists() {
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let looks_like_dir = !name.contains('.')
            || matches!(
                name,
                "plugins" | "rules" | "skills" | "memories" | "commands" | "agents"
            );
        if looks_like_dir {
            let _ = fs::create_dir_all(&target);
        }
    }
    if target.exists() {
        let _ = create_symlink(&target, &link);
    }
}

/// Recursively copy every file from `source` to `target`, WITHOUT overwriting existing
/// files (keep the shared-store copy on name collision). Used to preserve old sessions
/// when turning the account's real directory into a shared symlink.
fn merge_dir_into(source: &Path, target: &Path) {
    let _ = fs::create_dir_all(target);
    let Ok(entries) = fs::read_dir(source) else {
        return;
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let to = target.join(entry.file_name());
        // Use symlink_metadata so a symlink inside `source` is copied/skipped as a link, never
        // followed — a symlink pointing at `..` or `/` would otherwise recurse without bound.
        let Ok(meta) = from.symlink_metadata() else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        } else if meta.file_type().is_dir() {
            merge_dir_into(&from, &to);
        } else if !to.exists() {
            let _ = fs::copy(&from, &to);
        }
    }
}

/// Whether every file in `source` now exists at the matching path under `target` — i.e. the merge
/// lost nothing and `source` is safe to delete. A collision-skipped file present in both still
/// counts as present; a `source`-only file that failed to copy makes this false so the stash is kept.
///
/// Fails CLOSED: any read/metadata error returns false so the stash is preserved rather than deleting
/// data we couldn't verify was copied. Symlinks count as present (they aren't followed during merge,
/// so we don't require their target to exist).
fn dir_contents_all_present_in(source: &Path, target: &Path) -> bool {
    let Ok(entries) = fs::read_dir(source) else {
        return false; // can't verify → keep the stash
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let to = target.join(entry.file_name());
        let Ok(meta) = from.symlink_metadata() else {
            return false; // can't verify this entry → keep the stash
        };
        if meta.file_type().is_symlink() {
            continue; // not merged (links aren't followed); don't demand the target exists
        } else if meta.file_type().is_dir() {
            if !dir_contents_all_present_in(&from, &to) {
                return false;
            }
        } else if !to.exists() {
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Seed the "onboarding done" flag into the profile's `.claude.json` so interactive `claude`
/// does NOT run the first-time wizard (the wizard includes a "Select login method" step that
/// opens the browser and forces a re-login even when a token exists, plus "Choose text style").
/// Call this AFTER login finishes, because `claude auth login` overwrites `.claude.json` without this flag.
pub fn seed_onboarding(tool_id: &ToolId, profile: &Path) {
    if !matches!(tool_id, ToolId::Claude) {
        return;
    }
    let target = profile.join(".claude.json");
    let mut doc: serde_json::Value = fs::read_to_string(&target)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let Some(obj) = doc.as_object_mut() else {
        return;
    };

    let home_cfg: Option<serde_json::Value> = fs::read_to_string(home_dir().join(".claude.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let last_version = home_cfg
        .as_ref()
        .and_then(|c| c.get("lastOnboardingVersion"))
        .cloned()
        .or_else(|| crate::quota::claude_version().map(serde_json::Value::from))
        .unwrap_or_else(|| serde_json::Value::from("2.0.0"));
    let theme = home_cfg
        .as_ref()
        .and_then(|c| c.get("theme"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::from("dark"));

    obj.insert(
        "hasCompletedOnboarding".into(),
        serde_json::Value::Bool(true),
    );
    obj.insert("lastOnboardingVersion".into(), last_version);
    obj.entry("theme").or_insert(theme);

    if let Ok(text) = serde_json::to_string_pretty(&doc) {
        let _ = fs::write(&target, text);
    }
}

pub fn launch_profile_login(
    tool_id: &ToolId,
    store: &Store,
    account_id: &str,
    default_config_dir: &Path,
    binary_path: &Path,
) -> Result<()> {
    let profile = create_profile_with_default(tool_id, store, account_id, default_config_dir)?;
    link_shared_config_to(tool_id, &profile, default_config_dir);
    // Login only writes the token into the account's own config dir. It does NOT touch the
    // original `claude`/`codex` binaries. The account is used via its own command `claude-<name>`
    // (created by write_launcher); the original `claude` command is always the machine Default.
    match tool_id {
        ToolId::Claude => run_profile_login_command(
            binary_path,
            "CLAUDE_CONFIG_DIR",
            &profile,
            &["auth", "login"],
            tool_id,
        ),
        ToolId::Codex => run_profile_login_command(
            binary_path,
            "CODEX_HOME",
            &profile,
            &["login"],
            tool_id,
        ),
        ToolId::Antigravity => launch_login(tool_id),
    }
}

/// Whether the account has finished logging in — a token already exists for this config dir.
/// Used for the background poll after opening the Terminal login.
pub fn profile_has_credentials(tool_id: &ToolId, config_dir: &Path) -> bool {
    match tool_id {
        ToolId::Claude => {
            let suffix = crate::quota::claude_keychain_suffix(config_dir);
            keychain_service_exists(&format!("Claude Code-credentials-{suffix}"))
        }
        // Codex stores the token in a file inside CODEX_HOME.
        ToolId::Codex => config_dir.join("auth.json").exists(),
        // Antigravity IDE: the token lives in the account's own userData state.vscdb.
        ToolId::Antigravity => antigravity_logged_in(config_dir),
    }
}

fn keychain_service_exists(service: &str) -> bool {
    Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .output()
        .map(|out| out.status.success() && !out.stdout.is_empty())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Per-account launchers (Claude/Codex).
//
// Do NOT wrap the original `claude`/`codex` binaries (that once broke PATH). Instead
// each account has its OWN command `claude-<name>` / `codex-<name>` placed in
// ~/.local/bin, which hard-codes the config dir then execs the real binary:
//
//   #!/bin/sh
//   # ai-account-switcher-launcher v1
//   export CLAUDE_CONFIG_DIR='/.../accounts/claude/<id>'
//   exec '/Users/.../.local/bin/claude' "$@"
//
// The original `claude` command is never touched → it's always the machine Default.
// ---------------------------------------------------------------------------

const LAUNCHER_MARKER: &str = "# ai-account-switcher-launcher v1";

fn launcher_dir() -> PathBuf {
    home_dir().join(".local/bin")
}

pub fn launcher_path(name: &str) -> PathBuf {
    launcher_dir().join(name)
}

fn launcher_prefix(tool_id: &ToolId) -> Result<&'static str> {
    match tool_id {
        ToolId::Claude => Ok("claude-"),
        ToolId::Codex => Ok("codex-"),
        ToolId::Antigravity => anyhow::bail!("Antigravity is a GUI app and has no custom command"),
    }
}

/// Normalize + validate the command name the user entered. Enforce the `claude-`/`codex-`
/// prefix, allow only `a-z 0-9 - _`. Returns the full name (with prefix).
pub fn full_launcher_name(tool_id: &ToolId, raw: &str) -> Result<String> {
    let prefix = launcher_prefix(tool_id)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("Command name is empty");
    }
    // Allow the user to type it with or without the prefix.
    let body = trimmed.strip_prefix(prefix).unwrap_or(trimmed);
    if body.is_empty() {
        anyhow::bail!("Command name is empty");
    }
    if !body
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        anyhow::bail!("Command name may only contain lowercase a-z, digits, - or _");
    }
    if body.len() > 40 {
        anyhow::bail!("Command name is limited to 40 characters");
    }
    Ok(format!("{prefix}{body}"))
}

/// true if the file at path is a launcher created by the app (has the marker).
pub fn is_our_launcher(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|text| text.contains(LAUNCHER_MARKER))
        .unwrap_or(false)
}

/// Whether a command named `full_name` already exists in PATH that is NOT one of the
/// app's launchers (to avoid overwriting system binaries like `git`, `node`).
pub fn launcher_name_collides_with_system(full_name: &str) -> bool {
    match command_path(full_name) {
        Some(path) => !is_our_launcher(&path),
        None => false,
    }
}

/// Create/overwrite the launcher for the account. Execs the real binary by absolute path
/// (the ~/.local/bin/<binary> symlink stays stable across auto-update).
pub fn write_launcher(
    tool_id: &ToolId,
    store: &Store,
    account_id: &str,
    full_name: &str,
    real_binary: &Path,
) -> Result<()> {
    let env_name = match tool_id {
        ToolId::Claude => "CLAUDE_CONFIG_DIR",
        ToolId::Codex => "CODEX_HOME",
        ToolId::Antigravity => anyhow::bail!("Antigravity doesn't support custom commands"),
    };
    let profile = store.account_dir(tool_id, account_id);

    let dir = launcher_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(full_name);
    let script = format!(
        "#!/bin/sh\n{marker}\nexport {env}={dir}\nexec {bin} \"$@\"\n",
        marker = LAUNCHER_MARKER,
        env = env_name,
        dir = shell_quote(&profile.to_string_lossy()),
        bin = shell_quote(&real_binary.to_string_lossy()),
    );
    fs::write(&path, script)?;
    set_owner_executable_permissions(&path)?;
    Ok(())
}

/// Delete the launcher (only if it really is one of the app's launchers).
pub fn remove_launcher(name: &str) {
    let path = launcher_path(name);
    if path.exists() && is_our_launcher(&path) {
        let _ = fs::remove_file(path);
    }
}

// ---------------------------------------------------------------------------
// API / proxy accounts (Codex) — run the CLI through an external gateway with an API key
// instead of a subscription OAuth login. The account is a normal profile dir, but instead
// of `auth.json` it holds a `config.toml` (model_provider = proxy) plus an `api_key` file.
// Switch/launcher reuse the same machinery; the only extra is exporting the API key.
// ---------------------------------------------------------------------------

/// The file holding the raw API key inside an account's profile dir (chmod 600).
fn api_key_path(profile: &Path) -> PathBuf {
    profile.join("api_key")
}

/// Normalize a gateway base URL: trim whitespace and any trailing slash.
fn normalize_base_url(base_url: &str) -> String {
    base_url.trim().trim_end_matches('/').to_string()
}

/// List the gateway's available models, returning the `data[].id` strings. Tries `{base}/models`
/// first, then `{base}/v1/models` (Claude's base URL often omits the `/v1` that the models
/// endpoint needs). Reuses the app's curl helper (same as quota reads).
pub fn fetch_gateway_models(base_url: &str, api_key: &str) -> Result<Vec<String>> {
    let base = normalize_base_url(base_url);
    let mut candidates = vec![format!("{base}/models")];
    if !base.ends_with("/v1") {
        candidates.push(format!("{base}/v1/models"));
    }
    let auth = format!("Bearer {}", api_key.trim());

    let mut last_err: Option<anyhow::Error> = None;
    for url in candidates {
        match crate::quota::curl_get(&url, &[("Authorization", auth.as_str())]) {
            Ok(body) => {
                let value: serde_json::Value = serde_json::from_str(&body)
                    .context("The gateway returned an unexpected response")?;
                let ids = value
                    .get("data")
                    .and_then(|data| data.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| item.get("id").and_then(|id| id.as_str()))
                            .map(|id| id.to_string())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if ids.is_empty() {
                    anyhow::bail!("The gateway returned no models");
                }
                return Ok(ids);
            }
            Err(err) => last_err = Some(err),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Couldn't fetch models")))
        .context("Couldn't fetch models — check the gateway URL and API key")
}

/// Write the API key into the profile dir (owner read/write only).
pub fn write_api_key_file(profile: &Path, api_key: &str) -> Result<()> {
    fs::create_dir_all(profile)?;
    let path = api_key_path(profile);
    fs::write(&path, api_key.trim().as_bytes())?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

/// Escape a value for a TOML basic string (between double quotes).
fn toml_basic_string(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Write the Codex `config.toml` for an API/proxy account: point the provider at the gateway
/// and pin the model. `provider_name` is shown by Codex `/status`. Written once at creation;
/// Codex may later append its own keys (project trust, etc.) — the launcher re-forces the model
/// via `-m`, so config drift doesn't break the account.
pub fn write_codex_proxy_config(
    profile: &Path,
    provider_name: &str,
    base_url: &str,
    model: &str,
) -> Result<()> {
    fs::create_dir_all(profile)?;
    let mut toml = String::new();
    toml.push_str("# Managed by AI Account Switcher — API/proxy account.\n");
    toml.push_str(&format!("model = {}\n", toml_basic_string(model)));
    toml.push_str("model_provider = \"proxy\"\n\n");
    toml.push_str("[model_providers.proxy]\n");
    toml.push_str(&format!("name = {}\n", toml_basic_string(provider_name)));
    toml.push_str(&format!(
        "base_url = {}\n",
        toml_basic_string(&normalize_base_url(base_url))
    ));
    toml.push_str("wire_api = \"responses\"\n");
    toml.push_str("env_key = \"OPENAI_API_KEY\"\n");
    toml.push_str("supports_websockets = false\n\n");
    toml.push_str("[notice]\n");
    toml.push_str("hide_full_access_warning = true\n");
    fs::write(profile.join("config.toml"), toml)?;
    Ok(())
}

/// Write the Claude Code `settings.json` for an API/proxy account: Claude reads `env` from the
/// config dir's settings and applies the gateway base URL, bearer token, and model. Codex uses a
/// config.toml + key file instead; Claude is env-driven, so everything lives here.
pub fn write_claude_proxy_settings(
    profile: &Path,
    base_url: &str,
    token: &str,
    model: &str,
) -> Result<()> {
    fs::create_dir_all(profile)?;
    let settings = serde_json::json!({
        "env": {
            "ANTHROPIC_BASE_URL": normalize_base_url(base_url),
            "ANTHROPIC_AUTH_TOKEN": token.trim(),
            "ANTHROPIC_MODEL": model,
        }
    });
    let path = profile.join("settings.json");
    fs::write(&path, serde_json::to_string_pretty(&settings)?)?;
    set_owner_only_permissions(&path)?;
    Ok(())
}

/// Create/overwrite the launcher for an API/proxy account.
/// - Codex: export the key + force the gateway model with `-m` (Codex rewrites `model` in config
///   when the user touches its picker, so pinning on the command line keeps it working).
/// - Claude: auth/base/model come from the profile's settings.json env, so the launcher only sets
///   the config dir.
/// `bypass` adds the tool's skip-approvals flag.
pub fn write_api_launcher(
    tool_id: &ToolId,
    store: &Store,
    account_id: &str,
    full_name: &str,
    model: &str,
    bypass: bool,
    real_binary: &Path,
) -> Result<()> {
    let env_name = match tool_id {
        ToolId::Codex => "CODEX_HOME",
        ToolId::Claude => "CLAUDE_CONFIG_DIR",
        ToolId::Antigravity => anyhow::bail!("Antigravity doesn't support API/proxy accounts"),
    };
    let profile = store.account_dir(tool_id, account_id);

    // Codex pins the key + model on the command line; Claude carries them in settings.json env.
    let key_export = match tool_id {
        ToolId::Codex => format!(
            "export OPENAI_API_KEY=\"$(cat {})\"\n",
            shell_quote(&api_key_path(&profile).to_string_lossy())
        ),
        _ => String::new(),
    };
    let model_flag = match tool_id {
        ToolId::Codex => format!(" -m {}", shell_quote(model)),
        _ => String::new(),
    };
    let bypass_flag = match (tool_id, bypass) {
        (ToolId::Codex, true) => " --dangerously-bypass-approvals-and-sandbox",
        (ToolId::Claude, true) => " --dangerously-skip-permissions",
        _ => "",
    };

    let dir = launcher_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(full_name);
    let script = format!(
        "#!/bin/sh\n{marker}\nexport {env}={dir}\n{key}exec {bin}{model}{flag} \"$@\"\n",
        marker = LAUNCHER_MARKER,
        env = env_name,
        dir = shell_quote(&profile.to_string_lossy()),
        key = key_export,
        bin = shell_quote(&real_binary.to_string_lossy()),
        model = model_flag,
        flag = bypass_flag,
    );
    fs::write(&path, script)?;
    set_owner_executable_permissions(&path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// "Active" account for the BARE `claude`/`codex` commands — via a shell hook, NOT by
// wrapping the binary. The app writes the selected profile's path into the active file;
// an idempotent hook block in ~/.zshrc (+ ~/.bashrc if present) reads that file and
// exports CLAUDE_CONFIG_DIR/CODEX_HOME for every new shell. A per-account launcher
// (claude-b) exports its own override so it's unaffected. Default account = delete the
// active file → the bare command uses ~/.claude.
// ---------------------------------------------------------------------------

const HOOK_BEGIN: &str = "# >>> ai-account-switcher >>>";
const HOOK_END: &str = "# <<< ai-account-switcher <<<";

/// Write the account's profile into the active file (the bare command points here).
pub fn write_active_profile(tool_id: &ToolId, store: &Store, account_id: &str) -> Result<()> {
    let profile = store.account_dir(tool_id, account_id);
    if !profile.exists() {
        anyhow::bail!("Profile doesn't exist yet — has the account finished logging in?");
    }
    let active_path = store.active_profile_path(tool_id);
    if let Some(parent) = active_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(active_path, profile.to_string_lossy().as_bytes())?;
    Ok(())
}

/// Delete the active file (selecting machine Default → the bare command uses the original config dir).
pub fn clear_active_profile(tool_id: &ToolId, store: &Store) -> Result<()> {
    let active_path = store.active_profile_path(tool_id);
    if active_path.exists() {
        fs::remove_file(active_path)?;
    }
    Ok(())
}

/// Install (idempotently) the hook block into the shell rc so the bare command follows the selected account.
/// Called on every switch — cheap and self-healing if the user accidentally deletes it.
pub fn install_shell_hook(store: &Store) -> Result<()> {
    let claude_active = store.active_profile_path(&ToolId::Claude);
    let codex_active = store.active_profile_path(&ToolId::Codex);
    // `aisw` is a shell function: it re-reads the active file on EVERY call then exports. Used to
    // sync the new account into an already-open terminal without needing `source ~/.zshrc`.
    // At shell startup we call it once → a new terminal lands on the right account automatically.
    let block = format!(
        "{begin}\n\
         aisw() {{\n\
        \x20 if [ -r {claude} ]; then export CLAUDE_CONFIG_DIR=\"$(cat {claude})\"; else unset CLAUDE_CONFIG_DIR; fi\n\
        \x20 if [ -r {codex} ]; then export CODEX_HOME=\"$(cat {codex})\"; [ -r \"$CODEX_HOME/api_key\" ] && export OPENAI_API_KEY=\"$(cat \"$CODEX_HOME/api_key\")\"; else unset CODEX_HOME; fi\n\
        \x20 [ -n \"$1\" ] && echo \"AI Account Switcher: synced the account for this terminal.\"\n\
         }}\n\
         aisw >/dev/null 2>&1\n\
         {end}\n",
        begin = HOOK_BEGIN,
        end = HOOK_END,
        claude = shell_quote(&claude_active.to_string_lossy()),
        codex = shell_quote(&codex_active.to_string_lossy()),
    );

    let home = home_dir();
    // zsh is the default shell on macOS; add bash if the user has ~/.bashrc.
    let mut targets = vec![home.join(".zshrc")];
    if home.join(".bashrc").exists() {
        targets.push(home.join(".bashrc"));
    }
    for rc in targets {
        upsert_block(&rc, &block)?;
    }
    Ok(())
}

/// Replace the block between the markers (if present) or append; create the file if it doesn't exist.
fn upsert_block(rc: &Path, block: &str) -> Result<()> {
    let current = fs::read_to_string(rc).unwrap_or_default();
    let next = if let (Some(start), Some(end)) = (current.find(HOOK_BEGIN), current.find(HOOK_END))
    {
        let end = end + HOOK_END.len();
        let mut out = String::with_capacity(current.len());
        out.push_str(&current[..start]);
        out.push_str(block.trim_end());
        out.push_str(&current[end..]);
        out
    } else {
        let mut out = current;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(block);
        out
    };
    fs::write(rc, next)?;
    Ok(())
}

pub fn delete_account_files(tool_id: &ToolId, store: &Store, account_id: &str) -> Result<()> {
    let account_dir = store.account_dir(tool_id, account_id);

    // Claude stores the keychain token by the config dir's hash → delete it too so no
    // orphaned credential remains after deleting the account.
    if matches!(tool_id, ToolId::Claude) {
        let suffix = crate::quota::claude_keychain_suffix(&account_dir);
        let _ = Command::new("security")
            .args([
                "delete-generic-password",
                "-s",
                &format!("Claude Code-credentials-{suffix}"),
            ])
            .output();
    }

    if account_dir.exists() {
        fs::remove_dir_all(account_dir)?;
    }
    // Antigravity (copy-swap) stores its secret in the app's own keychain.
    for key in keychain_entries(tool_id) {
        let username = account_secret_username(tool_id, account_id, &key);
        if let Ok(entry) = Entry::new(APP_KEYCHAIN_SERVICE, &username) {
            let _ = entry.delete_credential();
        }
    }
    Ok(())
}

fn run_login_command(binary: &str, args: &[&str]) -> Result<()> {
    let command = command_path(binary).unwrap_or_else(|| PathBuf::from(binary));
    let mut script = shell_quote(&command.to_string_lossy());
    for arg in args {
        script.push(' ');
        script.push_str(&shell_quote(arg));
    }
    script.push_str("; echo; echo 'After signing in, return to AI Account Switcher.'");

    open_terminal_script(&script)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_name_enforces_prefix_and_charset() {
        // typed without the prefix → prefix gets added
        assert_eq!(
            full_launcher_name(&ToolId::Claude, "abc").unwrap(),
            "claude-abc"
        );
        // typed with the prefix → kept as-is
        assert_eq!(
            full_launcher_name(&ToolId::Codex, "codex-work").unwrap(),
            "codex-work"
        );
        // invalid characters → error
        assert!(full_launcher_name(&ToolId::Claude, "a b").is_err());
        assert!(full_launcher_name(&ToolId::Claude, "ABC").is_err());
        assert!(full_launcher_name(&ToolId::Claude, "").is_err());
        // antigravity is not supported
        assert!(full_launcher_name(&ToolId::Antigravity, "x").is_err());
    }
}

fn run_profile_login_command(
    command: &Path,
    env_name: &str,
    profile: &Path,
    login_args: &[&str],
    tool_id: &ToolId,
) -> Result<()> {
    let args = login_args
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "echo '=== Sign in to {tool}: follow the prompts, approve in your browser ==='; export {env}={dir}; {cmd} {args}; echo; echo 'Done — return to AI Account Switcher (it will detect it); you can close this window.'",
        tool = tool_id.display_name(),
        env = env_name,
        dir = shell_quote(&profile.to_string_lossy()),
        cmd = shell_quote(&command.to_string_lossy()),
        args = args,
    );
    open_terminal_script(&script)
}

fn open_terminal_script(script: &str) -> Result<()> {
    Command::new("osascript")
        .arg("-e")
        .arg(r#"tell application "Terminal" to activate"#)
        .arg("-e")
        .arg(format!(
            r#"tell application "Terminal" to do script "{}""#,
            script.replace('"', "\\\"")
        ))
        .spawn()
        .context("Couldn't open Terminal to sign in")?;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Clone)]
struct KeychainKey {
    service: String,
    username: String,
}

fn keychain_entries(tool_id: &ToolId) -> Vec<KeychainKey> {
    match tool_id {
        ToolId::Claude => {
            let username = current_username();
            vec![
                KeychainKey {
                    service: "Claude Code-credentials".to_string(),
                    username,
                },
                KeychainKey {
                    service: "Claude Code-credentials".to_string(),
                    username: "Claude Code".to_string(),
                },
                KeychainKey {
                    service: "Claude Code-credentials".to_string(),
                    username: "default".to_string(),
                },
            ]
        }
        ToolId::Codex => vec![],
        // Antigravity IDE switches via --user-data-dir (the login lives in each userData's
        // state.vscdb) so there's NO need to swap the keychain.
        ToolId::Antigravity => vec![],
    }
}

fn account_secret_username(tool_id: &ToolId, account_id: &str, key: &KeychainKey) -> String {
    format!(
        "{}:{}:{}:{}",
        tool_id.as_str(),
        account_id,
        key.service,
        key.username
    )
}

pub fn command_path(binary: &str) -> Option<PathBuf> {
    let path_output = Command::new("/usr/bin/which").arg(binary).output().ok();
    if let Some(output) = path_output {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !text.is_empty() {
                return Some(PathBuf::from(text));
            }
        }
    }

    common_bin_dirs()
        .into_iter()
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.exists())
}

pub(crate) fn common_bin_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        home.join("Library/pnpm"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".antigravity-ide/antigravity-ide/bin"),
        home.join(".antigravity/antigravity/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]
}

pub(crate) fn current_username() -> String {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            Command::new("/usr/bin/whoami")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

fn fingerprint(bytes: &[u8]) -> String {
    let hex = bytes
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("fp:{hex}")
}

pub(crate) fn home_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(unix)]
fn set_owner_executable_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_executable_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Owner read/write only (0600) — for the stored API key.
#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

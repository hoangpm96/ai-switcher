//! Cleanup for the REMOVED auto-session-prime daemons.
//!
//! Older versions installed up to two root LaunchDaemons: a pmset wake helper (woke the Mac before
//! a scheduled prime) and a prime daemon (ran the app headless every minute). The feature is gone,
//! but a machine that had it enabled still carries those daemons — the prime daemon would keep
//! launching the app binary every 60s, and the wake helper keeps a stale `pmset` wake armed. This
//! module only detects leftovers and tears them down (one admin prompt); it can no longer install
//! anything.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Labels + paths for the legacy daemons. The plists live in the system LaunchDaemons dir; the
/// helper script + bookkeeping live in root-owned /usr/local/libexec.
const HELPER_LABEL_BASE: &str = "dev.hoangphan.ai-account-switcher.wake-helper";
const PRIME_DAEMON_LABEL_BASE: &str = "dev.hoangphan.ai-account-switcher.prime-daemon";
const LEGACY_HELPER_PLIST_PATH: &str =
    "/Library/LaunchDaemons/dev.hoangphan.ai-account-switcher.wake-helper.plist";
const LEGACY_HELPER_SCRIPT_PATH: &str =
    "/usr/local/libexec/dev.hoangphan.ai-account-switcher-wake-helper.sh";
const LEGACY_HELPER_LAST_PATH: &str =
    "/usr/local/libexec/dev.hoangphan.ai-account-switcher-wake-last.txt";

fn current_uid() -> Result<String> {
    std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .context("couldn't determine the login user id")
}

fn helper_plist_path(uid: &str) -> PathBuf {
    PathBuf::from(format!(
        "/Library/LaunchDaemons/{HELPER_LABEL_BASE}.{uid}.plist"
    ))
}

fn prime_plist_path(uid: &str) -> PathBuf {
    PathBuf::from(format!(
        "/Library/LaunchDaemons/{PRIME_DAEMON_LABEL_BASE}.{uid}.plist"
    ))
}

fn helper_script_path(uid: &str) -> PathBuf {
    PathBuf::from(format!(
        "/usr/local/libexec/dev.hoangphan.ai-account-switcher-wake-helper-{uid}.sh"
    ))
}

fn helper_last_path(uid: &str) -> PathBuf {
    PathBuf::from(format!(
        "/usr/local/libexec/dev.hoangphan.ai-account-switcher-wake-last-{uid}.txt"
    ))
}

/// True if ANY of the legacy daemons (per-user wake helper, per-user prime daemon, or the pre-0.5.7
/// global helper) is still installed — the UI shows a one-tap cleanup when this is true.
pub fn legacy_daemons_installed() -> bool {
    let per_user = current_uid()
        .map(|uid| helper_plist_path(&uid).exists() || prime_plist_path(&uid).exists())
        .unwrap_or(false);
    per_user || std::path::Path::new(LEGACY_HELPER_PLIST_PATH).exists()
}

/// Remove every legacy daemon (one admin prompt): boot both per-user daemons out, delete their
/// plists + the root-owned helper script/bookkeeping, cancel any pmset wake the helper had armed,
/// and clean up the pre-0.5.7 global helper too.
pub fn uninstall_legacy_daemons() -> Result<()> {
    let uid = current_uid()?;
    let helper_plist = helper_plist_path(&uid);
    let prime_plist = prime_plist_path(&uid);
    let script_path = helper_script_path(&uid);
    let last_path = helper_last_path(&uid);
    let shell = [
        // Cancel the wake we set, if any, before removing the bookkeeping file.
        format!(
            "if [ -s {last} ]; then OLD=\"$(/bin/cat {last})\"; [ -n \"$OLD\" ] && /usr/bin/pmset schedule cancel wake \"$OLD\" 2>/dev/null; fi",
            last = sh_single_quote(&last_path.to_string_lossy())
        ),
        format!(
            "/bin/launchctl bootout system {} 2>/dev/null",
            sh_single_quote(&helper_plist.to_string_lossy())
        ),
        format!(
            "/bin/rm -f {}",
            sh_single_quote(&helper_plist.to_string_lossy())
        ),
        format!(
            "/bin/rm -f {}",
            sh_single_quote(&script_path.to_string_lossy())
        ),
        format!(
            "/bin/rm -f {}",
            sh_single_quote(&last_path.to_string_lossy())
        ),
        format!(
            "/bin/launchctl bootout system {} 2>/dev/null",
            sh_single_quote(&prime_plist.to_string_lossy())
        ),
        format!(
            "/bin/rm -f {}",
            sh_single_quote(&prime_plist.to_string_lossy())
        ),
        // Also remove the legacy pre-0.5.7 global helper if present.
        format!(
            "if [ -s {last} ]; then OLD=\"$(/bin/cat {last})\"; [ -n \"$OLD\" ] && /usr/bin/pmset schedule cancel wake \"$OLD\" 2>/dev/null; fi",
            last = sh_single_quote(LEGACY_HELPER_LAST_PATH)
        ),
        format!(
            "/bin/launchctl bootout system {} 2>/dev/null",
            sh_single_quote(LEGACY_HELPER_PLIST_PATH)
        ),
        format!(
            "/bin/rm -f {} {} {}",
            sh_single_quote(LEGACY_HELPER_PLIST_PATH),
            sh_single_quote(LEGACY_HELPER_SCRIPT_PATH),
            sh_single_quote(LEGACY_HELPER_LAST_PATH)
        ),
    ]
    .join("; ");
    run_as_admin(
        &shell,
        "AI Account Switcher cần quyền admin để gỡ các daemon auto-prime cũ",
    )
}

/// Single-quote a string for safe embedding in a `/bin/sh` command (wrap in '...' and escape any
/// embedded single quote as '\'').
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run a shell command as root via a single Finder admin prompt. Returns an error if the user
/// cancels or the command fails.
fn run_as_admin(shell_command: &str, prompt: &str) -> Result<()> {
    // Escape embedded double-quotes for the AppleScript string.
    let escaped = shell_command.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        "do shell script \"{escaped}\" with prompt \"{prompt}\" with administrator privileges"
    );
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .context("launching osascript for admin prompt")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        if err.contains("-128") || err.to_lowercase().contains("cancel") {
            anyhow::bail!("Bạn đã hủy cấp quyền admin");
        }
        anyhow::bail!("Không gỡ được daemon: {}", err.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_paths_are_isolated_per_macos_user() {
        assert_ne!(helper_plist_path("501"), helper_plist_path("502"));
        assert_ne!(prime_plist_path("501"), prime_plist_path("502"));
        assert_ne!(helper_script_path("501"), helper_script_path("502"));
    }
}

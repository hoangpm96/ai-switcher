//! In-app updates, the way Reqwise Figma does them: the app asks the store for `latest.json` a little
//! after start and then every few hours. A newer version shows a card with what's new; one click
//! downloads the package, replaces the app and reopens it, and the reopened app says which version it
//! is now on.
//!
//! Trust: every package is signed with the release key that only lives on the release Mac
//! (`~/.config/ai-switcher/updater.key`). The app checks that signature against the public key built
//! into it (`plugins.updater.pubkey` in `tauri.conf.json`) before it installs anything, so whoever
//! controls the download server still cannot ship a package of their own. `scripts/publish.mjs`
//! builds, signs and uploads the files.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::app_state::ManagedState;

/// Pushed to every window whenever the update state changes.
pub const EVENT: &str = "update-changed";
/// Leave the start-up path (quota reads, tray) alone before the first check.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// One request (the manifest, or the whole package) may take this long. The package is ~13 MB.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Test hook: read the manifest from another https URL instead — the `--test` channel of
/// `scripts/publish.mjs`, so a release can be tried end to end without offering it to anyone.
const ENDPOINT_ENV: &str = "AI_SWITCHER_UPDATE_ENDPOINT";
/// The app can't be replaced where it runs from (the mounted .dmg). The UI recognises "read-only" and
/// says it in the user's language.
const READ_ONLY: &str = "The app is running from a read-only place (the disk image?). Drag AI Account Switcher to Applications, open it from there, then update.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    /// Nothing to do: up to date, or not checked yet.
    #[default]
    Idle,
    Available,
    Downloading,
    /// Downloaded and verified; the app is being replaced and reopened.
    Installing,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    /// What's new: a JSON list of `{ "en", "vi" }` lines from `release-notes.json`.
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// The version running now.
    pub current: String,
    pub phase: UpdatePhase,
    /// The newer version on offer (phase available / downloading / installing).
    pub info: Option<UpdateInfo>,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub checking: bool,
    pub checked_at: Option<String>,
    /// Why the last check or install failed. Cleared by the next attempt.
    pub error: Option<String>,
    /// The app was just reopened by an update: the version it installed and its notes, shown once
    /// until the user closes it (`update_seen`).
    pub just_updated: Option<UpdateInfo>,
}

pub struct Updates(Mutex<UpdateState>);

/// Manage the update state and start the background checks. Call once from `setup`.
pub fn init(app: &AppHandle) {
    let current = app.package_info().version.to_string();
    let marker = app.state::<ManagedState>().store.update_marker_path();
    let just_updated = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|text| serde_json::from_str::<UpdateInfo>(&text).ok());
    // The marker only counts for the version it names: an update that failed to replace the app
    // (or a later manual install) must not claim "updated to vX".
    let just_updated = match just_updated {
        Some(info) if info.version == current => Some(info),
        _ => {
            let _ = std::fs::remove_file(&marker);
            None
        }
    };
    app.manage(Updates(Mutex::new(UpdateState {
        current,
        just_updated,
        ..Default::default()
    })));

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let _ = check(&handle).await;
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

pub fn status(app: &AppHandle) -> UpdateState {
    snapshot(app)
}

/// Ask the server for a newer version now. Errors are kept in the state (and returned as the
/// state), not raised, so a background check never surfaces as an error toast.
pub async fn check(app: &AppHandle) -> UpdateState {
    if busy(app) {
        return snapshot(app);
    }
    edit(app, |s| {
        s.checking = true;
        s.error = None;
    });
    let found = match updater(app, None) {
        Ok(updater) => updater.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    edit(app, |s| {
        s.checking = false;
        s.checked_at = Some(chrono::Utc::now().to_rfc3339());
        // An install may have started while this check was out.
        if matches!(s.phase, UpdatePhase::Downloading | UpdatePhase::Installing) {
            return;
        }
        match found {
            Ok(Some(update)) => {
                s.phase = UpdatePhase::Available;
                s.info = Some(info_of(&update));
            }
            Ok(None) => {
                s.phase = UpdatePhase::Idle;
                s.info = None;
            }
            // Keep what was on offer: a flaky network shouldn't hide an update already found.
            Err(e) => s.error = Some(e),
        }
    });
    snapshot(app)
}

/// Download the newer version, check its signature, replace the app and reopen it. Only returns
/// on failure — on success the app restarts.
pub async fn install(app: &AppHandle) -> Result<(), String> {
    if busy(app) {
        return Err("An update is already being installed".into());
    }
    // Work out where the new version goes before downloading anything: a copy that can't be
    // replaced should say so now, not after a 14 MB download.
    let target = match install_target() {
        Ok(target) => target,
        Err(e) => {
            edit(app, |s| s.error = Some(e.clone()));
            return Err(e);
        }
    };
    edit(app, |s| {
        s.phase = UpdatePhase::Downloading;
        s.downloaded = 0;
        s.total = None;
        s.error = None;
    });
    let result = download_and_install(app, target.as_ref()).await;
    match result {
        Ok(info) => {
            // Tell the reopened app what it was updated to. Best effort: without it the app just
            // doesn't show the "updated" note.
            let marker = app.state::<ManagedState>().store.update_marker_path();
            if let Ok(text) = serde_json::to_string(&info) {
                let _ = std::fs::write(marker, text);
            }
            // Free the single-instance socket first so the new process can claim it.
            tauri_plugin_single_instance::destroy(app);
            match target {
                Some(target) => {
                    // The package was checked against the updater key; don't let a leftover
                    // "downloaded from the internet" flag translocate the new copy again.
                    let _ = std::process::Command::new("/usr/bin/xattr")
                        .args(["-dr", "com.apple.quarantine"])
                        .arg(&target.app)
                        .status();
                    if target.translocated {
                        // This process runs from the read-only translocation mount, so restarting
                        // its own binary would reopen the old copy. Open the updated original once
                        // this process is gone.
                        let _ = std::process::Command::new("/bin/sh")
                            .arg("-c")
                            .arg("sleep 2; /usr/bin/open \"$0\"")
                            .arg(&target.app)
                            .spawn();
                        app.exit(0);
                        return Ok(());
                    }
                    app.restart();
                }
                None => app.restart(),
            }
        }
        Err(e) => {
            edit(app, |s| {
                s.phase = if s.info.is_some() { UpdatePhase::Available } else { UpdatePhase::Idle };
                s.error = Some(e.clone());
            });
            Err(e)
        }
    }
}

/// The installed copy an update replaces.
struct InstallTarget {
    /// The `.app` bundle.
    app: PathBuf,
    /// Its executable: the updater replaces the bundle holding the path it is given.
    exe: PathBuf,
    /// The running copy is a translocated one (see `translocation`).
    translocated: bool,
}

/// Where the new version must go. `Ok(None)` outside an app bundle (a dev build): the updater then
/// uses its own default. `Err(READ_ONLY)` when that place can't be written at all.
///
/// macOS runs a quarantined app that was not moved by Finder (opened straight from the .dmg, or
/// copied by a script) from a read-only "translocated" mount; replacing that path fails with
/// "Read-only file system (os error 30)". The copy the user actually has is the original location,
/// so that is the one to replace.
fn install_target() -> Result<Option<InstallTarget>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let Some(running) = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
    else {
        return Ok(None);
    };
    let inside = exe.strip_prefix(&running).map(Path::to_path_buf).ok();
    let (bundle, translocated) = match translocation::original(&running) {
        Some(original) => (original, true),
        None => (running, false),
    };
    let exe = inside.map(|rel| bundle.join(rel)).unwrap_or(exe);

    if let Some(parent) = bundle.parent() {
        let probe = parent.join(format!(".ai-switcher-update-probe-{}", std::process::id()));
        match std::fs::File::create(&probe) {
            Ok(_) => {
                let _ = std::fs::remove_file(&probe);
            }
            // EROFS: a mounted disk image. Permission denied is fine: the updater asks for an
            // administrator password then.
            Err(e) if e.raw_os_error() == Some(30) => return Err(READ_ONLY.into()),
            Err(_) => {}
        }
    }
    Ok(Some(InstallTarget {
        app: bundle,
        exe,
        translocated,
    }))
}

#[cfg(target_os = "macos")]
mod translocation {
    use core_foundation::base::TCFType;
    use core_foundation::url::{CFURL, CFURLRef};
    use std::ffi::c_void;
    use std::path::{Path, PathBuf};

    // Security.framework, public since macOS 10.12 (SecTranslocate.h).
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecTranslocateIsTranslocatedURL(path: CFURLRef, is_translocated: *mut bool, error: *mut *const c_void) -> u8;
        fn SecTranslocateCreateOriginalPathForURL(path: CFURLRef, error: *mut *const c_void) -> CFURLRef;
    }

    /// Where a translocated bundle really lives; `None` when `app` is not translocated.
    pub fn original(app: &Path) -> Option<PathBuf> {
        let url = CFURL::from_path(app, true)?;
        let mut translocated = false;
        // SAFETY: `url` is a valid CFURL for the call; a null error pointer is allowed.
        let ok = unsafe {
            SecTranslocateIsTranslocatedURL(url.as_concrete_TypeRef(), &mut translocated, std::ptr::null_mut())
        };
        if ok == 0 || !translocated {
            return None;
        }
        // SAFETY: as above; the result follows the Create rule, so we own it.
        let raw = unsafe { SecTranslocateCreateOriginalPathForURL(url.as_concrete_TypeRef(), std::ptr::null_mut()) };
        if raw.is_null() {
            return None;
        }
        unsafe { CFURL::wrap_under_create_rule(raw) }.to_path()
    }
}

#[cfg(not(target_os = "macos"))]
mod translocation {
    pub fn original(_: &std::path::Path) -> Option<std::path::PathBuf> {
        None
    }
}

/// The user closed the "updated to vX" note.
pub fn seen(app: &AppHandle) -> UpdateState {
    let marker = app.state::<ManagedState>().store.update_marker_path();
    let _ = std::fs::remove_file(marker);
    edit(app, |s| s.just_updated = None);
    snapshot(app)
}

async fn download_and_install(app: &AppHandle, target: Option<&InstallTarget>) -> Result<UpdateInfo, String> {
    let update = updater(app, target.map(|t| t.exe.as_path()))?
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "This version is already the newest".to_string())?;
    let info = info_of(&update);
    edit(app, |s| s.info = Some(info.clone()));

    let progress_app = app.clone();
    let finished_app = app.clone();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    update
        .download_and_install(
            move |chunk, total| {
                let emit = last_emit.elapsed() >= Duration::from_millis(200);
                if emit {
                    last_emit = Instant::now();
                }
                edit_quiet(&progress_app, |s| {
                    s.downloaded += chunk as u64;
                    s.total = total;
                });
                if emit {
                    let _ = progress_app.emit(EVENT, snapshot(&progress_app));
                }
            },
            move || edit(&finished_app, |s| s.phase = UpdatePhase::Installing),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(info)
}

fn updater(app: &AppHandle, exe: Option<&Path>) -> Result<tauri_plugin_updater::Updater, String> {
    let mut builder = app.updater_builder().timeout(REQUEST_TIMEOUT);
    if let Some(exe) = exe {
        builder = builder.executable_path(exe);
    }
    if let Ok(url) = std::env::var(ENDPOINT_ENV) {
        let url = url
            .parse::<tauri::Url>()
            .map_err(|e| format!("{ENDPOINT_ENV}: {e}"))?;
        builder = builder.endpoints(vec![url]).map_err(|e| e.to_string())?;
    }
    builder.build().map_err(|e| e.to_string())
}

fn info_of(update: &tauri_plugin_updater::Update) -> UpdateInfo {
    UpdateInfo {
        version: update.version.clone(),
        notes: update.body.clone(),
        date: update
            .raw_json
            .get("pub_date")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    }
}

fn busy(app: &AppHandle) -> bool {
    matches!(
        snapshot(app).phase,
        UpdatePhase::Downloading | UpdatePhase::Installing
    )
}

fn snapshot(app: &AppHandle) -> UpdateState {
    app.state::<Updates>()
        .0
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

fn edit_quiet(app: &AppHandle, f: impl FnOnce(&mut UpdateState)) {
    if let Ok(mut s) = app.state::<Updates>().0.lock() {
        f(&mut s);
    }
}

fn edit(app: &AppHandle, f: impl FnOnce(&mut UpdateState)) {
    edit_quiet(app, f);
    let _ = app.emit(EVENT, snapshot(app));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_that_was_not_translocated_has_no_original() {
        // Also proves the Security.framework symbols link and run on this macOS.
        assert_eq!(translocation::original(Path::new("/System/Applications/Calculator.app")), None);
        assert_eq!(translocation::original(Path::new("/nonexistent/X.app")), None);
    }
}

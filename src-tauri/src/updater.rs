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
    let found = match updater(app) {
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
    edit(app, |s| {
        s.phase = UpdatePhase::Downloading;
        s.downloaded = 0;
        s.total = None;
        s.error = None;
    });
    let result = download_and_install(app).await;
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
            app.restart();
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

/// The user closed the "updated to vX" note.
pub fn seen(app: &AppHandle) -> UpdateState {
    let marker = app.state::<ManagedState>().store.update_marker_path();
    let _ = std::fs::remove_file(marker);
    edit(app, |s| s.just_updated = None);
    snapshot(app)
}

async fn download_and_install(app: &AppHandle) -> Result<UpdateInfo, String> {
    let update = updater(app)?
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

fn updater(app: &AppHandle) -> Result<tauri_plugin_updater::Updater, String> {
    let mut builder = app.updater_builder().timeout(REQUEST_TIMEOUT);
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

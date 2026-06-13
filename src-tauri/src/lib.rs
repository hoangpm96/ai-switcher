mod app_state;
mod detection;
mod models;
mod pricing;
mod quota;
mod store;
mod tools;
mod tray;
mod usage;

use app_state::ManagedState;
use models::{
    AddAccountInput, AddApiAccountInput, AppSnapshot, DetectionReport, RenameAccountInput,
    SetLauncherInput, SetToolSetupInput, SwitchAccountInput, ToolId, UsageReport,
};
use tauri::{Emitter, Manager, State};

#[tauri::command]
fn load_snapshot(state: State<'_, ManagedState>) -> Result<AppSnapshot, String> {
    // App just opened: update accounts that finished logging in while it was closed.
    let _ = state.recheck_pending_logins();
    state.snapshot().map_err(display_error)
}

#[tauri::command]
async fn refresh_tool(app: tauri::AppHandle, tool_id: ToolId) -> Result<AppSnapshot, String> {
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        app2.state::<ManagedState>()
            .refresh_tool(tool_id, Some(&app2))
            .map_err(display_error)
    })
    .await
    .map_err(|e| e.to_string())?;
    tray::rebuild(&app);
    result
}

#[tauri::command]
async fn refresh_account(
    app: tauri::AppHandle,
    tool_id: ToolId,
    account_id: String,
) -> Result<AppSnapshot, String> {
    let app2 = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        app2.state::<ManagedState>()
            .refresh_single_account(&tool_id, &account_id, Some(&app2))
            .map_err(display_error)
    })
    .await
    .map_err(|e| e.to_string())?;
    tray::rebuild(&app);
    result
}

#[tauri::command]
fn add_account(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    input: AddAccountInput,
) -> Result<AppSnapshot, String> {
    let snapshot = state.add_account(&app, input).map_err(display_error)?;
    tray::rebuild(&app);
    Ok(snapshot)
}

#[tauri::command]
fn add_api_account(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    input: AddApiAccountInput,
) -> Result<AppSnapshot, String> {
    let snapshot = state.add_api_account(input).map_err(display_error)?;
    tray::rebuild(&app);
    Ok(snapshot)
}

/// List the gateway's models (`{base_url}/models`) so the Add dialog can offer a default model +
/// mapping targets. Stateless — just proxies the HTTP call.
#[tauri::command]
fn fetch_gateway_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    tools::fetch_gateway_models(&base_url, &api_key).map_err(display_error)
}

#[tauri::command]
fn rename_account(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    input: RenameAccountInput,
) -> Result<AppSnapshot, String> {
    let snapshot = state.rename_account(input).map_err(display_error)?;
    tray::rebuild(&app);
    Ok(snapshot)
}

#[tauri::command]
fn switch_account(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    input: SwitchAccountInput,
) -> Result<AppSnapshot, String> {
    let snapshot = state.switch_account(input).map_err(display_error)?;
    tray::rebuild(&app);
    Ok(snapshot)
}

#[tauri::command]
fn set_launcher(
    state: State<'_, ManagedState>,
    input: SetLauncherInput,
) -> Result<AppSnapshot, String> {
    state.set_launcher(input).map_err(display_error)
}

#[tauri::command]
fn delete_account(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    tool_id: ToolId,
    account_id: String,
) -> Result<AppSnapshot, String> {
    let snapshot = state
        .delete_account(tool_id, account_id)
        .map_err(display_error)?;
    tray::rebuild(&app);
    Ok(snapshot)
}

#[tauri::command]
fn accept_disclaimer(state: State<'_, ManagedState>) -> Result<AppSnapshot, String> {
    state.accept_disclaimer().map_err(display_error)
}

#[tauri::command]
fn antigravity_new_login(state: State<'_, ManagedState>) -> Result<AppSnapshot, String> {
    state.antigravity_new_login().map_err(display_error)
}

#[tauri::command]
fn set_auto_switch(
    state: State<'_, ManagedState>,
    enabled: bool,
    threshold: f64,
) -> Result<AppSnapshot, String> {
    state
        .set_auto_switch(enabled, threshold)
        .map_err(display_error)
}

#[tauri::command]
fn set_auto_switch_setting(
    state: State<'_, ManagedState>,
    tool_id: ToolId,
    enabled: bool,
    threshold: f64,
) -> Result<AppSnapshot, String> {
    state
        .set_auto_switch_setting(tool_id, enabled, threshold)
        .map_err(display_error)
}

#[tauri::command]
fn detect_tool_setup(state: State<'_, ManagedState>, tool_id: ToolId) -> DetectionReport {
    state.detect_tool_setup(tool_id)
}

#[tauri::command]
fn validate_tool_setup(
    state: State<'_, ManagedState>,
    input: SetToolSetupInput,
) -> DetectionReport {
    state.validate_tool_setup(input)
}

#[tauri::command]
fn set_tool_setup(
    state: State<'_, ManagedState>,
    input: SetToolSetupInput,
) -> Result<AppSnapshot, String> {
    state.set_tool_setup(input).map_err(display_error)
}

/// Token usage + cost report for the Usage tab (Claude + Codex, aggregated per tool).
/// `range_days` limits the totals to the last N local days (0 = all time).
#[tauri::command]
fn get_usage(state: State<'_, ManagedState>, range_days: u32) -> UsageReport {
    state.usage_report(range_days)
}

pub fn run() {
    let state = ManagedState::new().expect("failed to initialize app state");
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            load_snapshot,
            refresh_tool,
            refresh_account,
            add_account,
            add_api_account,
            fetch_gateway_models,
            rename_account,
            switch_account,
            set_launcher,
            delete_account,
            accept_disclaimer,
            antigravity_new_login,
            set_auto_switch,
            set_auto_switch_setting,
            detect_tool_setup,
            validate_tool_setup,
            set_tool_setup,
            get_usage
        ])
        .setup(|app| {
            // Menu-bar (tray) icon for quick account switching without opening the window.
            tray::create(app.handle())?;

            // Background poller: periodically refresh quota + auto-switch if enabled.
            // Refresh every 5 minutes (Claude's quota endpoint is rate-limited hard;
            // the 5h/weekly quota changes slowly, so no need to poll more often).
            let handle = app.handle().clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(300));
                let state = handle.state::<ManagedState>();
                for tool_id in [ToolId::Claude, ToolId::Codex] {
                    let _ = state.refresh_tool(tool_id, Some(&handle));
                }
                // Keep the token-usage cache warm and nudge any open Usage tab to refetch
                // (with whatever range the user has selected).
                let _ = state.usage_report(0);
                let _ = handle.emit("usage-changed", ());
                // Refresh the tray menu's quota %/checkmarks with the new snapshot.
                tray::rebuild(&handle);
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close (✕) hides the main window to the tray instead of quitting — the poller and
            // tray stay alive so quick-switch keeps working. Quit is via the tray menu.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // Clicking the Dock icon while the window is hidden (closed to tray) reopens it.
            if let tauri::RunEvent::Reopen { .. } = event {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        });
}

fn display_error(error: anyhow::Error) -> String {
    let text = error.to_string();
    if text.contains("Failed to switch account") {
        "Failed to switch account — kept the previous account".to_string()
    } else if text.contains("Login not completed") {
        "Login not completed, account not added".to_string()
    } else if text.contains("No login") {
        "No login found for this tool".to_string()
    } else {
        text
    }
}

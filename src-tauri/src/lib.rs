mod app_state;
mod models;
mod quota;
mod store;
mod tools;

use app_state::ManagedState;
use models::{
    AddAccountInput, AppSnapshot, RenameAccountInput, SetLauncherInput, SwitchAccountInput, ToolId,
};
use tauri::{Manager, State};

#[tauri::command]
fn load_snapshot(state: State<'_, ManagedState>) -> Result<AppSnapshot, String> {
    // App just opened: update accounts that finished logging in while it was closed.
    let _ = state.recheck_pending_logins();
    state.snapshot().map_err(display_error)
}

#[tauri::command]
fn refresh_tool(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    tool_id: ToolId,
) -> Result<AppSnapshot, String> {
    state
        .refresh_tool(tool_id, Some(&app))
        .map_err(display_error)
}

#[tauri::command]
fn add_account(
    app: tauri::AppHandle,
    state: State<'_, ManagedState>,
    input: AddAccountInput,
) -> Result<AppSnapshot, String> {
    state.add_account(&app, input).map_err(display_error)
}

#[tauri::command]
fn rename_account(
    state: State<'_, ManagedState>,
    input: RenameAccountInput,
) -> Result<AppSnapshot, String> {
    state.rename_account(input).map_err(display_error)
}

#[tauri::command]
fn switch_account(
    state: State<'_, ManagedState>,
    input: SwitchAccountInput,
) -> Result<AppSnapshot, String> {
    state.switch_account(input).map_err(display_error)
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
    state: State<'_, ManagedState>,
    tool_id: ToolId,
    account_id: String,
) -> Result<AppSnapshot, String> {
    state
        .delete_account(tool_id, account_id)
        .map_err(display_error)
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
            add_account,
            rename_account,
            switch_account,
            set_launcher,
            delete_account,
            accept_disclaimer,
            antigravity_new_login,
            set_auto_switch
        ])
        .setup(|app| {
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
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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

//! macOS menu-bar (tray) icon + native dropdown menu for quick account switching.
//!
//! Native `NSMenu` only renders text + a checkmark (no custom bars/badges). The active
//! account uses a real native checkmark (CheckMenuItem, kept enabled so it isn't greyed
//! out); every row trails its quota + plan as `· 96% · Plus`. Tools are grouped under a
//! disabled header with separators between them.
//!
//! Claude Code + Codex only (Antigravity switching restarts the IDE — too heavy here).
//! Rebuilt from a fresh snapshot whenever the app state changes via `rebuild`.

use crate::app_state::ManagedState;
use crate::models::{Account, AccountState, AppSnapshot, SwitchAccountInput, ToolId, ToolStatus};
use tauri::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Wry};

/// Tools shown in the tray, in order. Antigravity is intentionally excluded.
const TRAY_TOOLS: [ToolId; 2] = [ToolId::Claude, ToolId::Codex];

const SWITCH_PREFIX: &str = "switch:";
const OPEN_ID: &str = "tray:open";
const QUIT_ID: &str = "tray:quit";

/// Creates the tray icon once at startup.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;
    let snapshot = app.state::<ManagedState>().snapshot().ok();
    let menu = build_menu(app, snapshot.as_ref())?;

    let _tray = TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        // Monochrome template — macOS recolours it per theme (white in dark menu bar) and
        // brightens it when the menu is open, matching the system icons (wifi/clock).
        .icon_as_template(true)
        .tooltip("AI Account Switcher")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(handle_menu_event)
        .build(app)?;
    Ok(())
}

/// Rebuilds the menu from the current snapshot so checkmarks + quota stay in sync.
pub fn rebuild(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("main-tray") else {
        return;
    };
    let snapshot = app.state::<ManagedState>().snapshot().ok();
    if let Ok(menu) = build_menu(app, snapshot.as_ref()) {
        let _ = tray.set_menu(Some(menu));
    }
}

fn build_menu(app: &AppHandle, snapshot: Option<&AppSnapshot>) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;

    if let Some(snapshot) = snapshot {
        let mut any_tool = false;
        for tool_id in TRAY_TOOLS {
            let Some(tool) = snapshot.tools.iter().find(|t| t.id == tool_id) else {
                continue;
            };
            if !tool.installed {
                continue;
            }
            if any_tool {
                menu.append(&PredefinedMenuItem::separator(app)?)?;
            }
            any_tool = true;
            append_tool_section(app, &menu, tool)?;
        }
        if any_tool {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
    }

    menu.append(&MenuItem::with_id(app, OPEN_ID, "Open AI Switcher…", true, None::<&str>)?)?;
    menu.append(&MenuItem::with_id(app, QUIT_ID, "Quit", true, Some("Cmd+Q"))?)?;
    Ok(menu)
}

fn append_tool_section(app: &AppHandle, menu: &Menu<Wry>, tool: &ToolStatus) -> tauri::Result<()> {
    // Bold-ish tool header as a disabled row.
    menu.append(&MenuItem::with_id(
        app,
        format!("header:{}", tool.id.as_str()),
        tool.name.clone(),
        false,
        None::<&str>,
    )?)?;

    if tool.accounts.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            format!("empty:{}", tool.id.as_str()),
            "   No accounts",
            false,
            None::<&str>,
        )?)?;
        return Ok(());
    }

    for account in &tool.accounts {
        let is_active = Some(account.id.as_str()) == tool.active_account_id.as_deref()
            || account.state == AccountState::Active;
        let needs_login = account.state == AccountState::NeedsLogin;
        let id = format!("{SWITCH_PREFIX}{}:{}", tool.id.as_str(), account.id);
        let label = account_label(account);

        if is_active {
            // Native checkmark for the account in use. Kept ENABLED so it renders in the
            // normal (not greyed-out) text colour; clicking it just re-selects the same
            // account, which is a harmless no-op.
            menu.append(&CheckMenuItem::with_id(
                app, id, label, true, true, None::<&str>,
            )?)?;
        } else {
            menu.append(&MenuItem::with_id(app, id, label, !needs_login, None::<&str>)?)?;
        }
    }
    Ok(())
}

/// `Work  ·  96% · Plus` — name trailed by quota + plan (no status emoji).
fn account_label(account: &Account) -> String {
    let mut trailer: Vec<String> = Vec::new();
    if let Some(quota) = &account.quota {
        if let Some(percent) = quota.five_hour.percent_used {
            trailer.push(format!("{}%", percent.round() as i64));
        }
        if let Some(plan) = &quota.plan {
            trailer.push(plan.clone());
        }
    } else if account.api_provider.is_some() {
        trailer.push("API".to_string());
    }

    if trailer.is_empty() {
        account.name.clone()
    } else {
        format!("{}  ·  {}", account.name, trailer.join(" · "))
    }
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id();
    match id.as_ref() {
        OPEN_ID => show_main_window(app),
        QUIT_ID => app.exit(0),
        other if other.starts_with(SWITCH_PREFIX) => switch_from_id(app, id),
        _ => {}
    }
}

/// Parses `switch:<tool>:<accountId>` and switches on a worker thread (switching shells
/// out to the CLI, which must not block the menu event handler).
fn switch_from_id(app: &AppHandle, id: &MenuId) {
    let rest = &id.as_ref()[SWITCH_PREFIX.len()..];
    let Some((tool_str, account_id)) = rest.split_once(':') else {
        return;
    };
    let tool_id = match tool_str {
        "claude" => ToolId::Claude,
        "codex" => ToolId::Codex,
        _ => return,
    };
    let account_id = account_id.to_string();
    let app = app.clone();
    std::thread::spawn(move || {
        let result = app.state::<ManagedState>().switch_account(SwitchAccountInput {
            tool_id,
            account_id,
        });
        if let Ok(snapshot) = result {
            use tauri::Emitter;
            let _ = app.emit("snapshot-changed", &snapshot);
        }
        rebuild(&app);
    });
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

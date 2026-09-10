//! Always-on-top quota overlay: a second, frameless window that floats above every app so the
//! quota bars stay visible while the user is coding in a terminal/IDE.
//!
//! The window is created on demand (tray toggle, Settings switch, or restored at startup when it
//! was left on). It renders the same React bundle as the main window — `main.tsx` branches on the
//! window label — so there is no second HTML entry to keep in sync.
//!
//! Geometry lives in `StoredState.overlay.rect` and is written back from the move/resize handler
//! in `lib.rs`, which is why nothing here tries to remember positions itself.

use crate::app_state::ManagedState;
use crate::models::{OverlayRect, OverlaySettings};
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "overlay";

/// Opens the overlay (or focuses/reveals it if it already exists).
pub fn show(app: &AppHandle) -> tauri::Result<()> {
    let settings = app
        .state::<ManagedState>()
        .overlay_settings()
        .unwrap_or_default();

    if let Some(window) = app.get_webview_window(LABEL) {
        window.show()?;
        let _ = window.set_always_on_top(true);
        let _ = window.set_ignore_cursor_events(settings.click_through);
        return Ok(());
    }

    let rect = sanitize_rect(app, settings.rect);

    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("Quota")
        .inner_size(rect.width, rect.height)
        .position(rect.x, rect.y)
        .min_inner_size(190.0, 78.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .resizable(true)
        .skip_taskbar(true)
        .shadow(true)
        // Don't steal focus from the terminal/editor when the overlay appears.
        .focused(false)
        // First click acts on the control under the cursor instead of only activating the window.
        .accept_first_mouse(true)
        .build()?;

    // Keep it visible when the user switches Spaces / enters another full-screen app.
    let _ = window.set_visible_on_all_workspaces(true);
    let _ = window.set_ignore_cursor_events(settings.click_through);
    Ok(())
}

/// Closes the overlay window if it is open. Does not touch the `enabled` flag.
pub fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
}

/// Applies settings to the live window and tells both windows to re-render with them.
pub fn apply(app: &AppHandle, settings: &OverlaySettings) {
    if settings.enabled {
        let _ = show(app);
    } else {
        hide(app);
    }
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.set_ignore_cursor_events(settings.click_through);
    }
    let _ = app.emit("overlay-settings-changed", settings);
}

/// Keeps a restored position on an actually-connected monitor. Unplugging the external display the
/// overlay was parked on would otherwise reopen it somewhere invisible.
fn sanitize_rect(app: &AppHandle, rect: OverlayRect) -> OverlayRect {
    let mut rect = rect;
    rect.width = rect.width.clamp(190.0, 900.0);
    rect.height = rect.height.clamp(78.0, 900.0);

    let monitors = app.available_monitors().unwrap_or_default();
    if monitors.is_empty() {
        return rect;
    }

    // The title bar is invisible here, so require a decent slice of the window to be on-screen —
    // enough that the user can always grab it and drag it back.
    let visible = monitors.iter().any(|monitor| {
        let scale = monitor.scale_factor();
        let position: LogicalPosition<f64> = monitor.position().to_logical(scale);
        let size: LogicalSize<f64> = monitor.size().to_logical(scale);
        let overlap_x = (rect.x + rect.width).min(position.x + size.width) - rect.x.max(position.x);
        let overlap_y =
            (rect.y + rect.height).min(position.y + size.height) - rect.y.max(position.y);
        overlap_x >= 80.0 && overlap_y >= 40.0
    });

    if !visible {
        let primary = app
            .primary_monitor()
            .ok()
            .flatten()
            .or_else(|| monitors.first().cloned());
        if let Some(monitor) = primary {
            let scale = monitor.scale_factor();
            let position: LogicalPosition<f64> = monitor.position().to_logical(scale);
            rect.x = position.x + 40.0;
            rect.y = position.y + 60.0;
        } else {
            let fallback = OverlayRect::default();
            rect.x = fallback.x;
            rect.y = fallback.y;
        }
    }
    rect
}

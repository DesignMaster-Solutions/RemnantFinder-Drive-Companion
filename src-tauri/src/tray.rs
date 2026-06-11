use crate::{build_ui_state, mount_drive_internal, open_mount_internal, sync_now_internal, AppState, UiState};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    App, AppHandle, Manager, WebviewWindow,
};

#[derive(Debug, Clone, PartialEq)]
pub enum TrayState {
    Disconnected,
    Connected,
    Mounted,
    Syncing,
    Error,
    Offline,
}

impl TrayState {
    pub fn from_ui(state: &UiState) -> Self {
        if !state.logged_in {
            return TrayState::Disconnected;
        }
        if state.sync_phase == "syncing" {
            return TrayState::Syncing;
        }
        if state.sync_phase == "error" {
            return TrayState::Error;
        }
        if !state.online {
            return TrayState::Offline;
        }
        if state.mounted {
            return TrayState::Mounted;
        }
        TrayState::Connected
    }
}

/// Overlay a 4×4 colored dot (bottom-right corner) onto the base icon bytes.
/// `rgba` is (r, g, b, a). Works with 32×32 RGBA raw pixels (width=32).
fn overlay_dot(base_rgba: &[u8], width: u32, height: u32, rgba: (u8, u8, u8, u8)) -> Vec<u8> {
    let mut pixels = base_rgba.to_vec();
    let (r, g, b, a) = rgba;
    let dot_size: u32 = 6;
    let margin: u32 = 2;
    let x_start = width.saturating_sub(dot_size + margin);
    let y_start = height.saturating_sub(dot_size + margin);
    for dy in 0..dot_size {
        for dx in 0..dot_size {
            let x = x_start + dx;
            let y = y_start + dy;
            if x < width && y < height {
                let idx = ((y * width + x) * 4) as usize;
                if idx + 4 <= pixels.len() {
                    pixels[idx] = r;
                    pixels[idx + 1] = g;
                    pixels[idx + 2] = b;
                    pixels[idx + 3] = a;
                }
            }
        }
    }
    pixels
}

pub fn set_tray_icon(app: &AppHandle, state: &TrayState) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else { return };

    // On macOS the icon is rendered as template (monochrome) — dot overlay is the
    // only reliable way to indicate state without requiring separate asset files.
    let base = match app.default_window_icon() {
        Some(icon) => icon.clone(),
        None => return,
    };

    // Decode base icon to raw RGBA for pixel manipulation.
    let (width, height) = (base.width(), base.height());
    let rgba_base = base.rgba().to_vec();

    let dot_color: Option<(u8, u8, u8, u8)> = match state {
        TrayState::Mounted  => Some((34, 197, 94, 255)),   // green
        TrayState::Syncing  => Some((59, 130, 246, 255)),  // blue
        TrayState::Error    => Some((239, 68, 68, 255)),   // red
        TrayState::Offline  => Some((156, 163, 175, 255)), // gray
        TrayState::Connected => Some((234, 179, 8, 255)),  // yellow (ready, not mounted)
        TrayState::Disconnected => None,                   // plain icon — no dot
    };

    let final_rgba = match dot_color {
        Some(color) => overlay_dot(&rgba_base, width, height, color),
        None => rgba_base,
    };

    let icon = Image::new(&final_rgba, width, height);
    let _ = tray.set_icon(Some(icon));
}

pub struct TrayMenuHandles {
    open_mount: MenuItem<tauri::Wry>,
    mount: MenuItem<tauri::Wry>,
    unmount: MenuItem<tauri::Wry>,
    sync: MenuItem<tauri::Wry>,
    logout: MenuItem<tauri::Wry>,
}

pub const TRAY_ID: &str = "main";

const MENU_SHOW: &str = "show";
const MENU_OPEN_MOUNT: &str = "open_mount";
const MENU_MOUNT: &str = "mount";
const MENU_UNMOUNT: &str = "unmount";
const MENU_SYNC: &str = "sync";
const MENU_LOGOUT: &str = "logout";
const MENU_QUIT: &str = "quit";

pub fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let show_i = MenuItem::with_id(app, MENU_SHOW, "Open Stone Project Drive", true, None::<&str>)?;
    let open_mount_i =
        MenuItem::with_id(app, MENU_OPEN_MOUNT, "Open Mount Folder", true, None::<&str>)?;
    let mount_i = MenuItem::with_id(app, MENU_MOUNT, "Mount Drive", true, None::<&str>)?;
    let unmount_i = MenuItem::with_id(app, MENU_UNMOUNT, "Unmount Drive", false, None::<&str>)?;
    let sync_i = MenuItem::with_id(app, MENU_SYNC, "Sync Now", true, None::<&str>)?;
    let logout_i = MenuItem::with_id(app, MENU_LOGOUT, "Sign Out", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_i,
            &open_mount_i,
            &PredefinedMenuItem::separator(app)?,
            &mount_i,
            &unmount_i,
            &sync_i,
            &PredefinedMenuItem::separator(app)?,
            &logout_i,
            &PredefinedMenuItem::separator(app)?,
            &quit_i,
        ],
    )?;

    let icon = app
        .default_window_icon()
        .expect("app icon must be configured in tauri.conf.json")
        .clone();
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .tooltip("Stone Project Drive — Disconnected")
        .show_menu_on_left_click(false);

    // Do NOT set icon_as_template — we use colored dot overlays to indicate state,
    // which requires the icon to be rendered in color, not as a template (monochrome).
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(false);
    }

    builder
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            match id {
                MENU_SHOW => show_main_window(app),
                MENU_OPEN_MOUNT => {
                    if let Err(e) = open_mount_internal(app) {
                        tracing::error!("tray open mount failed: {e}");
                    }
                }
                MENU_MOUNT => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = mount_drive_internal(&app).await {
                            tracing::error!("tray mount failed: {e}");
                        }
                        refresh_tray(app);
                    });
                }
                MENU_UNMOUNT => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        let mount = state.mount.lock().take();
                        if let Some(handle) = mount {
                            if let Err(e) = handle.unmount().await {
                                tracing::error!("tray unmount failed: {e}");
                            }
                        }
                        refresh_tray(app);
                    });
                }
                MENU_SYNC => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = sync_now_internal(&app).await {
                            tracing::error!("tray sync failed: {e}");
                        }
                        refresh_tray(app);
                    });
                }
                MENU_LOGOUT => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(e) = logout_internal(&app).await {
                            tracing::error!("tray logout failed: {e}");
                        }
                        refresh_tray(app);
                    });
                }
                MENU_QUIT => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = logout_internal(&app).await;
                        app.exit(0);
                    });
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app.handle())?;

    app.manage(TrayMenuHandles {
        open_mount: open_mount_i,
        mount: mount_i,
        unmount: unmount_i,
        sync: sync_i,
        logout: logout_i,
    });

    Ok(())
}

pub fn refresh_tray(app: AppHandle) {
    let state = match build_ui_state(&app) {
        Ok(state) => state,
        Err(_) => {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_tooltip(Some("Stone Project Drive"));
            }
            return;
        }
    };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(&tray_tooltip(&state)));
    }

    // Update icon to reflect current state
    let tray_state = TrayState::from_ui(&state);
    set_tray_icon(&app, &tray_state);

    if let Some(handles) = app.try_state::<TrayMenuHandles>() {
        let _ = handles.open_mount.set_enabled(state.logged_in && state.mounted);
        let _ = handles.mount.set_enabled(state.logged_in && !state.mounted);
        let _ = handles.unmount.set_enabled(state.logged_in && state.mounted);
        let _ = handles.sync.set_enabled(state.logged_in);
        let _ = handles.logout.set_enabled(state.logged_in);
    }
}

pub fn hide_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    set_dock_visible(app, false);
}

pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    set_dock_visible(app, true);
}

pub fn should_start_hidden() -> bool {
    std::env::args().any(|arg| arg == "--minimized")
}

pub fn attach_window_close_handler(window: &WebviewWindow) {
    let app = window.app_handle().clone();
    window.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            hide_main_window(&app);
        }
    });
}

fn tray_tooltip(state: &UiState) -> String {
    if !state.logged_in {
        return "Stone Project Drive — Disconnected".to_string();
    }

    if state.sync_phase == "syncing" {
        let detail = if state.sync_detail.is_empty() {
            "Syncing…".to_string()
        } else {
            state.sync_detail.clone()
        };
        return format!("Stone Project Drive — {detail}");
    }

    if !state.online {
        return "Stone Project Drive — Offline (pinned folders only)".to_string();
    }

    if state.mounted {
        return format!(
            "Stone Project Drive — {} · {}",
            state.mount_point, state.last_sync_human
        );
    }

    format!(
        "Stone Project Drive — {} · Sign in & mount",
        state.company_name
    )
}

async fn logout_internal(app: &AppHandle) -> Result<(), String> {
    use drive_core::auth;

    let state = app.state::<AppState>();
    let mount = state.mount.lock().take();
    if let Some(mount) = mount {
        mount.unmount().await.map_err(|e| e.to_string())?;
    }
    *state.sync.lock() = None;
    *state.logged_in.lock() = false;
    auth::clear_credentials().map_err(|e| e.to_string())?;
    Ok(())
}

fn set_dock_visible(app: &AppHandle, visible: bool) {
    #[cfg(target_os = "macos")]
    {
        use tauri::ActivationPolicy;
        let policy = if visible {
            ActivationPolicy::Regular
        } else {
            ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
    }

    let _ = (app, visible);
}

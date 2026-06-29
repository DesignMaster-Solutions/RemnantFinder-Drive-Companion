mod notify;
mod tray;

use drive_core::api::DriveApiClient;
use drive_core::auth;
use drive_core::cache::CacheStore;
use drive_core::config::AppConfig;
use drive_core::sync::SyncEngine;
use drive_core::vfs::{mount_drive as vfs_mount_drive, MountHandle};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::MacosLauncher;
use notify::{notify_mount_failure, notify_mount_success, notify_sync_error};
use tray::{attach_window_close_handler, hide_main_window, refresh_tray, setup_tray, should_start_hidden};

pub struct AppState {
    config: Mutex<AppConfig>,
    sync: Mutex<Option<SyncEngine>>,
    mount: Mutex<Option<MountHandle>>,
    logged_in: Mutex<bool>,
}

#[derive(Serialize, Clone)]
pub struct UiState {
    logged_in: bool,
    mounted: bool,
    mount_point: String,
    api_base_url: String,
    company_name: String,
    user_email: String,
    pinned_paths: Vec<String>,
    cache_usage_human: String,
    auto_start: bool,
    auto_mount: bool,
    online: bool,
    sync_phase: String,
    sync_detail: String,
    last_sync_human: String,
    app_version: String,
}

#[derive(Serialize, Clone)]
pub struct DriveFolderEntry {
    name: String,
    path: String,
}

#[derive(Serialize, Clone)]
pub struct UpdateCheckResult {
    current_version: String,
    latest_version: String,
    update_available: bool,
    download_url: Option<String>,
    release_notes: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct PairStartResult {
    user_code: String,
    verification_uri: String,
    device_code: String,
    interval: u64,
}

fn profile_from_login(data: &serde_json::Value) -> (String, String) {
    let user = data.get("user");
    let email = user
        .and_then(|u| u.get("email"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let company = user
        .and_then(|u| u.get("company"))
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Your company")
        .to_string();
    (email, company)
}

pub(crate) fn build_ui_state(app: &AppHandle) -> Result<UiState, String> {
    use tauri_plugin_autostart::ManagerExt;

    let state = app.state::<AppState>();
    let logged_in = *state.logged_in.lock();
    let mounted = state.mount.lock().is_some();
    let mut pinned_paths = Vec::new();
    let mut cache_usage_human = String::new();
    let mut online = true;
    let mut sync_phase = "idle".to_string();
    let mut sync_detail = String::new();
    let mut last_sync_human = "Never synced yet".to_string();

    if let Some(sync) = state.sync.lock().clone() {
        pinned_paths = sync.pinned_paths().unwrap_or_default();
        cache_usage_human = sync.cache_usage_human().unwrap_or_default();
        online = sync.is_online_sync();
        let snapshot = sync.status_snapshot();
        last_sync_human = snapshot.last_sync_human();
        sync_phase = snapshot.phase;
        sync_detail = snapshot.detail;
    }

    let auto_start = app.autolaunch().is_enabled().map_err(|e| e.to_string())?;
    let config = state.config.lock();

    Ok(UiState {
        logged_in,
        mounted,
        mount_point: config.mount_point.clone(),
        api_base_url: config.api_base_url.clone(),
        company_name: config
            .company_name
            .clone()
            .unwrap_or_else(|| "Your company".to_string()),
        user_email: config.user_email.clone().unwrap_or_default(),
        pinned_paths,
        cache_usage_human,
        auto_start,
        auto_mount: config.auto_mount,
        online,
        sync_phase,
        sync_detail,
        last_sync_human,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

fn emit_status(app: &AppHandle) {
    if let Ok(ui) = build_ui_state(app) {
        let _ = app.emit("status-changed", ui);
    }
    refresh_tray(app.clone());
}

fn persist_config(config: &AppConfig) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())
}

fn build_sync(config: &AppConfig, token: &str, company_id: &str) -> anyhow::Result<SyncEngine> {
    let cache_dir = config.cache_path_for_company(company_id);
    let cache = CacheStore::open(&cache_dir)?;
    let api = DriveApiClient::new(&config.api_base_url, token, company_id);
    Ok(SyncEngine::new(api, cache))
}

pub async fn mount_drive_internal(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let sync = state
        .sync
        .lock()
        .clone()
        .ok_or_else(|| "not logged in".to_string())?;

    if state.mount.lock().is_some() {
        emit_status(app);
        return Ok(());
    }

    let (mount_point, webdav_port) = {
        let config = state.config.lock();
        (config.mount_point.clone(), config.webdav_port)
    };

    let handle = vfs_mount_drive(sync.clone(), &mount_point, webdav_port, true)
        .await
        .map_err(|e| {
            let message = e.to_string();
            notify_mount_failure(app, &message);
            message
        })?;

    *state.mount.lock() = Some(handle);
    notify_mount_success(app, &mount_point);
    emit_status(app);

    tauri::async_runtime::spawn(async move {
        if let Err(e) = sync.seed_root_tree().await {
            tracing::warn!("failed to seed drive tree: {e}");
        }
    });

    Ok(())
}

pub async fn sync_now_internal(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let sync = state
        .sync
        .lock()
        .clone()
        .ok_or_else(|| "not logged in".to_string())?;
    sync.sync_changes().await.map_err(|e| {
        let message = e.to_string();
        notify_sync_error(app, &message);
        message
    })?;
    emit_status(app);
    Ok(())
}

pub fn open_mount_internal(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let mount_point = app.state::<AppState>().config.lock().mount_point.clone();
    app.opener()
        .open_path(&mount_point, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn login(
    app: AppHandle,
    state: State<'_, AppState>,
    api_url: String,
    email: String,
    password: String,
    company_id: Option<String>,
) -> Result<(), String> {
    let api_url = api_url.trim().to_string();
    let email = email.trim().to_string();

    let mut config = state.config.lock().clone();
    config.api_base_url = api_url.clone();

    let (token, login_data) = DriveApiClient::login(&config.api_base_url, &email, &password)
        .await
        .map_err(|e| e.to_string())?;

    let company_id = match company_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => id.to_string(),
        None => DriveApiClient::company_id_from_login(&login_data).map_err(|e| e.to_string())?,
    };

    let (user_email, company_name) = profile_from_login(&login_data);
    config.company_id = Some(company_id.clone());
    config.user_email = Some(user_email);
    config.company_name = Some(company_name);

    auth::save_token(&token).map_err(|e| e.to_string())?;
    auth::save_company_id(&company_id).map_err(|e| e.to_string())?;

    let sync = build_sync(&config, &token, &company_id).map_err(|e| e.to_string())?;
    let background = sync.clone();
    tauri::async_runtime::spawn(async move {
        background.run_background_loop(120).await;
    });
    *state.sync.lock() = Some(sync);
    *state.logged_in.lock() = true;
    *state.config.lock() = config.clone();
    persist_config(&config)?;

    emit_status(&app);

    if config.auto_mount {
        mount_drive_internal(&app).await?;
    }

    Ok(())
}

/// Start device pairing — used by the "Sign in with your Stone Project account"
/// flow so Google/SSO users (no password) can authorize the companion from the
/// already-logged-in web app.
#[tauri::command]
async fn pair_start(state: State<'_, AppState>, api_url: String) -> Result<PairStartResult, String> {
    let api_url = api_url.trim().to_string();
    // Persist the chosen API URL so the subsequent poll uses the same base.
    {
        let mut config = state.config.lock();
        config.api_base_url = api_url.clone();
    }

    let data = DriveApiClient::pair_start(&api_url)
        .await
        .map_err(|e| e.to_string())?;

    let pick = |key: &str| {
        data.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let complete = pick("verification_uri_complete");
    let verification_uri = if complete.is_empty() {
        pick("verification_uri")
    } else {
        complete
    };

    Ok(PairStartResult {
        user_code: pick("user_code"),
        verification_uri,
        device_code: pick("device_code"),
        interval: data.get("interval").and_then(|v| v.as_u64()).unwrap_or(5),
    })
}

/// Poll a pending pairing. Returns "pending" until approved; on "approved" it
/// completes the session (token + company_id) exactly like the password login.
#[tauri::command]
async fn pair_poll(
    app: AppHandle,
    state: State<'_, AppState>,
    api_url: String,
    device_code: String,
) -> Result<String, String> {
    let api_url = api_url.trim().to_string();
    let data = DriveApiClient::pair_poll(&api_url, &device_code)
        .await
        .map_err(|e| e.to_string())?;

    let status = data
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("pending");
    if status != "approved" {
        return Ok("pending".to_string());
    }

    let token = data
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "approved pairing missing token".to_string())?
        .to_string();
    let company_id = data
        .get("company_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "approved pairing missing company_id".to_string())?
        .to_string();

    let mut config = state.config.lock().clone();
    config.api_base_url = api_url.clone();
    config.company_id = Some(company_id.clone());

    auth::save_token(&token).map_err(|e| e.to_string())?;
    auth::save_company_id(&company_id).map_err(|e| e.to_string())?;

    let sync = build_sync(&config, &token, &company_id).map_err(|e| e.to_string())?;
    let background = sync.clone();
    tauri::async_runtime::spawn(async move {
        background.run_background_loop(120).await;
    });
    *state.sync.lock() = Some(sync);
    *state.logged_in.lock() = true;
    *state.config.lock() = config.clone();
    persist_config(&config)?;

    emit_status(&app);

    if config.auto_mount {
        mount_drive_internal(&app).await?;
    }

    Ok("approved".to_string())
}

#[tauri::command]
async fn logout(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mount = state.mount.lock().take();
    if let Some(mount) = mount {
        mount.unmount().await.map_err(|e| e.to_string())?;
    }
    *state.sync.lock() = None;
    *state.logged_in.lock() = false;
    auth::clear_credentials().map_err(|e| e.to_string())?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
async fn mount_drive(app: AppHandle) -> Result<(), String> {
    mount_drive_internal(&app).await
}

#[tauri::command]
async fn unmount_drive(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mount = state.mount.lock().take();
    if let Some(handle) = mount {
        handle.unmount().await.map_err(|e| e.to_string())?;
    }
    emit_status(&app);
    Ok(())
}

#[tauri::command]
async fn sync_now(app: AppHandle) -> Result<(), String> {
    sync_now_internal(&app).await
}

#[tauri::command]
async fn pin_folder(app: AppHandle, state: State<'_, AppState>, path: String) -> Result<(), String> {
    let sync = state
        .sync
        .lock()
        .clone()
        .ok_or_else(|| "not logged in".to_string())?;
    sync.pin_folder(&path).await.map_err(|e| e.to_string())?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
async fn unpin_folder(app: AppHandle, state: State<'_, AppState>, path: String) -> Result<(), String> {
    let sync = state
        .sync
        .lock()
        .clone()
        .ok_or_else(|| "not logged in".to_string())?;
    sync.unpin_folder(&path).map_err(|e| e.to_string())?;
    emit_status(&app);
    Ok(())
}

#[tauri::command]
fn get_state(app: AppHandle) -> Result<UiState, String> {
    build_ui_state(&app)
}

#[tauri::command]
async fn list_drive_folders(
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<Vec<DriveFolderEntry>, String> {
    let sync = state
        .sync
        .lock()
        .clone()
        .ok_or_else(|| "not logged in".to_string())?;
    let path = path.unwrap_or_default();
    let nodes = sync.list_directory(&path).await.map_err(|e| e.to_string())?;
    Ok(nodes
        .into_iter()
        .filter(|node| node.is_folder())
        .map(|node| DriveFolderEntry {
            name: node.name,
            path: node.path,
        })
        .collect())
}

#[tauri::command]
async fn check_for_update(state: State<'_, AppState>) -> Result<UpdateCheckResult, String> {
    let api_base_url = state.config.lock().api_base_url.clone();
    let platform = if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "mac"
    };
    let current = env!("CARGO_PKG_VERSION").to_string();
    let url = format!(
        "{}/companion/version?platform={platform}",
        api_base_url.trim_end_matches('/')
    );

    let response = reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(12))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        return Ok(UpdateCheckResult {
            current_version: current.clone(),
            latest_version: current,
            update_available: false,
            download_url: None,
            release_notes: None,
        });
    }

    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let data = body
        .get("data")
        .cloned()
        .unwrap_or(body);

    let latest = data
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or(&current)
        .to_string();
    let download_url = data
        .get("download_url")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let release_notes = data
        .get("release_notes")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    Ok(UpdateCheckResult {
        update_available: is_newer_version(&latest, &current) && download_url.is_some(),
        current_version: current,
        latest_version: latest,
        download_url,
        release_notes,
    })
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse = |value: &str| {
        value
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let latest_parts = parse(latest);
    let current_parts = parse(current);
    for index in 0..3 {
        let l = latest_parts.get(index).copied().unwrap_or(0);
        let c = current_parts.get(index).copied().unwrap_or(0);
        if l > c {
            return true;
        }
        if l < c {
            return false;
        }
    }
    false
}

async fn mount_health_loop(app: AppHandle) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(25)).await;

        let (should_check, port) = {
            let state = app.state::<AppState>();
            if !*state.logged_in.lock() {
                continue;
            }
            let mounted = state.mount.lock().is_some();
            let port = state.config.lock().webdav_port;
            (mounted, port)
        };

        if !should_check {
            continue;
        }

        let alive = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok();
        if alive {
            continue;
        }

        tracing::warn!("webdav server not responding on port {port}, attempting remount");
        {
            let state = app.state::<AppState>();
            *state.mount.lock() = None;
        }

        if let Err(error) = mount_drive_internal(&app).await {
            notify_mount_failure(&app, &error);
        }
    }
}

#[tauri::command]
fn open_mount(app: AppHandle) -> Result<(), String> {
    open_mount_internal(&app)
}

#[tauri::command]
fn set_auto_start(app: AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let autostart = app.autolaunch();
    if enabled {
        autostart.enable().map_err(|e| e.to_string())?;
    } else {
        autostart.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn set_auto_mount(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    let mut config = state.config.lock();
    config.auto_mount = enabled;
    persist_config(&config)
}

#[tauri::command]
fn hide_window(app: AppHandle) -> Result<(), String> {
    hide_main_window(&app);
    Ok(())
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // A second launch focuses the existing window instead of starting a
            // rival instance that would fight over port 17817 and drive R:.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            let config = AppConfig::load();
            let mut logged_in = false;
            let mut sync = None;

            if let (Ok(token), Ok(company_id)) = (auth::load_token(), auth::load_company_id()) {
                if let Ok(engine) = build_sync(&config, &token, &company_id) {
                    sync = Some(engine);
                    logged_in = true;
                }
            }

            app.manage(AppState {
                config: Mutex::new(config.clone()),
                sync: Mutex::new(sync.clone()),
                mount: Mutex::new(None),
                logged_in: Mutex::new(logged_in),
            });

            if let Some(engine) = sync {
                tauri::async_runtime::spawn(async move {
                    engine.run_background_loop(30).await;
                });
            }

            setup_tray(app)?;

            if let Some(window) = app.get_webview_window("main") {
                attach_window_close_handler(&window);
            }

            // Always refresh tray icon so it reflects restored login state on startup
            refresh_tray(app.handle().clone());

            if should_start_hidden() {
                hide_main_window(&app.handle());
            }

            if logged_in && config.auto_mount {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = mount_drive_internal(&app_handle).await {
                        tracing::error!("auto-mount on startup failed: {e}");
                        notify_mount_failure(&app_handle, &e);
                    }
                    emit_status(&app_handle);
                });
            }

            let poll_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    emit_status(&poll_app);
                }
            });

            let health_app = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                mount_health_loop(health_app).await;
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            login,
            pair_start,
            pair_poll,
            logout,
            mount_drive,
            unmount_drive,
            sync_now,
            pin_folder,
            unpin_folder,
            list_drive_folders,
            check_for_update,
            get_state,
            open_mount,
            set_auto_start,
            set_auto_mount,
            hide_window,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

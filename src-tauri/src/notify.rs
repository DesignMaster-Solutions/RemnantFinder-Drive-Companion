use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;

pub fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(error) = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .show()
    {
        tracing::warn!("notification failed: {error}");
    }
}

pub fn notify_mount_success(app: &AppHandle, mount_point: &str) {
    notify(
        app,
        "Stone Project Drive mounted",
        &format!("Open files at {mount_point}"),
    );
}

pub fn notify_mount_failure(app: &AppHandle, message: &str) {
    notify(app, "Could not mount drive", message);
}

pub fn notify_sync_error(app: &AppHandle, message: &str) {
    notify(app, "Sync issue", message);
}

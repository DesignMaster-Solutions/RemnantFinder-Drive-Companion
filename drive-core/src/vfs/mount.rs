use crate::sync::SyncEngine;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// CREATE_NO_WINDOW — impede que spawns de processos de console (net.exe)
/// abram janelas de terminal piscando sobre a UI no Windows.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Append a timestamped line to `~/.remnant-finder/mount.log`. The GUI app has no
/// visible console, so this file is how we diagnose mount failures in the field.
/// Best-effort: any error is swallowed.
fn mount_log(msg: &str) {
    let Some(home) = dirs::home_dir() else { return };
    let dir = home.join(".remnant-finder");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("mount.log"))
    {
        use std::io::Write;
        let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{ts}] {msg}");
    }
}

pub enum MountBackend {
    WebDav,
    #[cfg(all(target_os = "macos", feature = "fuse"))]
    MacFuse,
    #[cfg(all(windows, feature = "winfsp"))]
    WinFsp,
}

pub struct MountHandle {
    pub backend: MountBackend,
    pub mount_point: String,
    pub webdav_port: Option<u16>,
    /// Signals the dedicated WebDAV server thread to stop. `None` for native backends.
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    /// Handle to the dedicated WebDAV server thread. `None` for native backends.
    server_thread: Option<std::thread::JoinHandle<()>>,
}

impl MountHandle {
    pub async fn unmount(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.server_thread.take() {
            // Join off the async runtime so we never block a worker thread.
            let _ = tokio::task::spawn_blocking(move || {
                let _ = thread.join();
            })
            .await;
        }
        unmount_drive(&self.mount_point, &self.backend).await
    }
}

pub async fn mount_drive(
    sync: SyncEngine,
    mount_point: &str,
    webdav_port: u16,
    prefer_native: bool,
) -> Result<MountHandle> {
    #[cfg(all(target_os = "macos", feature = "fuse"))]
    if prefer_native {
        if let Ok(handle) = mount_macfuse(sync.clone(), mount_point).await {
            return Ok(handle);
        }
    }

    #[cfg(all(windows, feature = "winfsp"))]
    if prefer_native {
        if let Ok(handle) = mount_winfsp(sync.clone(), mount_point).await {
            return Ok(handle);
        }
    }

    let _ = prefer_native;
    mount_webdav(sync, mount_point, webdav_port).await
}

pub async fn unmount_drive(mount_point: &str, backend: &MountBackend) -> Result<()> {
    match backend {
        MountBackend::WebDav => detach_webdav_mount(mount_point),
        #[cfg(all(target_os = "macos", feature = "fuse"))]
        MountBackend::MacFuse => detach_webdav_mount(mount_point),
        #[cfg(all(windows, feature = "winfsp"))]
        MountBackend::WinFsp => Ok(()),
    }
}

async fn mount_webdav(sync: SyncEngine, mount_point: &str, port: u16) -> Result<MountHandle> {
    mount_log(&format!("mount_webdav start: mount_point={mount_point} port={port}"));
    if is_mounted_at(mount_point) {
        tracing::info!("removing stale mount at {mount_point}");
        mount_log("found stale mapping at mount point — detaching");
        let _ = detach_webdav_mount(mount_point);
    }

    // Run the WebDAV server on its own OS thread with a dedicated runtime. Sharing
    // the UI's async runtime let WebView2 startup starve the accept loop, so the
    // server was often still unbound 400ms later when `net use` ran — Windows then
    // returned "system error 67". A dedicated thread binds immediately and stays
    // responsive regardless of UI-thread load.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<std::io::Result<()>>();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server = crate::webdav::WebDavServer::new(sync, port);

    let server_thread = std::thread::Builder::new()
        .name("webdav-server".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            runtime.block_on(server.serve_until_shutdown(ready_tx, shutdown_rx));
        })
        .map_err(|e| anyhow!("failed to spawn WebDAV server thread: {e}"))?;

    // Wait until the server has actually bound the port before mounting. The local
    // server can take several seconds to come up on a cold start; we wait for the
    // real signal instead of blindly sleeping and racing `net use`.
    match tokio::time::timeout(std::time::Duration::from_secs(30), ready_rx).await {
        Ok(Ok(Ok(()))) => {
            mount_log(&format!("WebDAV server bound and ready on 127.0.0.1:{port}"));
        }
        Ok(Ok(Err(e))) => {
            mount_log(&format!("server bind FAILED on 127.0.0.1:{port}: {e}"));
            let _ = shutdown_tx.send(());
            return Err(anyhow!(
                "local WebDAV server could not start on 127.0.0.1:{port}: {e}"
            ));
        }
        Ok(Err(_)) => {
            mount_log("server thread ended before signalling readiness");
            return Err(anyhow!("local WebDAV server stopped before it finished starting"));
        }
        Err(_) => {
            mount_log("server did not become ready within 30s (timeout)");
            let _ = shutdown_tx.send(());
            return Err(anyhow!(
                "local WebDAV server did not become ready within 30s"
            ));
        }
    }

    if let Err(e) = attach_webdav_mount(mount_point, port).await {
        mount_log(&format!("attach (net use) ultimately FAILED: {e}"));
        // Stop the server so the port is freed for the next attempt (no leak).
        let _ = shutdown_tx.send(());
        let _ = tokio::task::spawn_blocking(move || {
            let _ = server_thread.join();
        })
        .await;
        return Err(e);
    }

    mount_log(&format!("MOUNTED ok at {mount_point}"));
    Ok(MountHandle {
        backend: MountBackend::WebDav,
        mount_point: mount_point.to_string(),
        webdav_port: Some(port),
        shutdown: Some(shutdown_tx),
        server_thread: Some(server_thread),
    })
}

#[cfg(target_os = "macos")]
fn is_mounted_at(mount_point: &str) -> bool {
    let Ok(output) = Command::new("mount").output() else {
        return false;
    };
    let mount_list = String::from_utf8_lossy(&output.stdout);
    mount_list.lines().any(|line| {
        line.contains(&format!(" on {mount_point} "))
            || line.contains(&format!(" on {mount_point}("))
    })
}

#[cfg(windows)]
fn is_mounted_at(mount_point: &str) -> bool {
    let Ok(output) = Command::new("net")
        .args(["use"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let listing = String::from_utf8_lossy(&output.stdout);
    let needle = mount_point.trim_end_matches('\\');
    listing.lines().any(|line| {
        let line = line.trim();
        line.starts_with(needle) || line.contains(&format!(" {needle} "))
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn is_mounted_at(_mount_point: &str) -> bool {
    false
}

fn is_windows_drive_letter(mount_point: &str) -> bool {
    let trimmed = mount_point.trim().trim_end_matches('\\');
    trimmed.len() == 2
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
        && trimmed.as_bytes()[1] == b':'
}

fn ensure_mount_directory(mount_point: &str) -> Result<()> {
    if is_windows_drive_letter(mount_point) {
        return Ok(());
    }

    let path = Path::new(mount_point);
    if path.exists() {
        if path.is_dir() {
            return Ok(());
        }
        return Err(anyhow!(
            "mount path exists but is not a directory: {mount_point}"
        ));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("failed to create mount parent {}: {e}", parent.display()))?;
    }

    std::fs::create_dir_all(path)
        .map_err(|e| anyhow!("failed to create mount directory {mount_point}: {e}"))?;
    Ok(())
}

async fn attach_webdav_mount(mount_point: &str, port: u16) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/drive");
    ensure_mount_directory(mount_point)?;

    // The Windows WebDAV redirector caches a "server not found" result for ~60s
    // (ServerNotFoundCacheLifeTimeInSec) after any failed attempt, returning
    // "system error 67" instantly during that window even once the server is up.
    // Retry past that window so a transient early failure self-heals.
    const ATTEMPTS: usize = 16;
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=ATTEMPTS {
        let url = url.clone();
        let mp = mount_point.to_string();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || run_mount_webdav_command(&url, &mp)),
        )
        .await
        .map_err(|_| anyhow!("mount timed out after 30 seconds"))?
        .map_err(|e| anyhow!("mount task failed: {e}"))?;

        match result {
            Ok(()) => {
                mount_log(&format!("net use attempt {attempt}/{ATTEMPTS}: OK"));
                return Ok(());
            }
            Err(e) => {
                mount_log(&format!("net use attempt {attempt}/{ATTEMPTS}: {e}"));
                tracing::warn!("mount attempt {attempt}/{ATTEMPTS} failed: {e}");
                last_err = Some(e);
                if attempt < ATTEMPTS {
                    // Ramp quickly then hold at 7s so the loop spans >60s overall.
                    let backoff = (attempt as u64).min(7);
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow!("mount failed")))
}

#[cfg(target_os = "macos")]
fn run_mount_webdav_command(url: &str, mount_point: &str) -> Result<()> {
    let output = Command::new("/sbin/mount_webdav")
        .args([
            "-S",
            "-v",
            "Stone Project",
            "-o",
            "nobrowse",
            url,
            mount_point,
        ])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(anyhow!(
            "mount_webdav failed: {}{}",
            stderr.trim(),
            stdout.trim()
        ));
    }
    Ok(())
}

/// Convert `http://host:port/path` to the WebDAV UNC form `\\host@port\path`.
/// Windows' WebDAV redirector rejects an http:// URL in `net use` (system
/// error 67 "the network name cannot be found"); it needs the UNC form.
#[cfg(windows)]
fn http_url_to_webdav_unc(url: &str) -> String {
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let (host, port) = match authority.find(':') {
        Some(i) => (&authority[..i], &authority[i + 1..]),
        None => (authority, ""),
    };
    let path_bs = path.replace('/', "\\");
    if port.is_empty() {
        format!("\\\\{host}\\{path_bs}")
    } else {
        format!("\\\\{host}@{port}\\{path_bs}")
    }
}

#[cfg(windows)]
fn run_mount_webdav_command(url: &str, mount_point: &str) -> Result<()> {
    let unc = http_url_to_webdav_unc(url);

    // Best-effort: nudge the WebClient (WebDAV redirector) service awake. It is
    // trigger-started on first DAV access, so this usually isn't required and
    // may fail without admin — ignore the result either way.
    let _ = Command::new("net")
        .args(["start", "WebClient"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    let output = Command::new("net")
        .args(["use", mount_point, &unc, "/persistent:no"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = format!("{} {}", stdout.trim(), stderr.trim());
        return Err(anyhow!("net use {mount_point} {unc} failed: {}", detail.trim()));
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", windows)))]
fn run_mount_webdav_command(_url: &str, _mount_point: &str) -> Result<()> {
    Err(anyhow!("automatic mount not supported on this platform"))
}

fn detach_webdav_mount(mount_point: &str) -> Result<()> {
    if !is_mounted_at(mount_point) {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let umount = Command::new("umount").arg(mount_point).output();
        if umount.as_ref().is_ok_and(|o| o.status.success()) {
            return Ok(());
        }
        let output = Command::new("diskutil")
            .args(["unmount", mount_point])
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        return Err(anyhow!(
            "unmount failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    #[cfg(windows)]
    {
        let output = Command::new("net")
            .args(["use", mount_point, "/delete", "/y"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        return Err(anyhow!(
            "net use /delete failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = mount_point;
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "fuse"))]
async fn mount_macfuse(sync: SyncEngine, mount_point: &str) -> Result<MountHandle> {
    use fuser::{Filesystem, MountOption};
    use std::ffi::OsStr;
    use std::time::{Duration, SystemTime};

    struct DriveFs {
        sync: SyncEngine,
    }

    impl Filesystem for DriveFs {
        fn lookup(&mut self, _parent: u64, name: &OsStr, reply: fuser::ReplyEntry) {
            let _ = (name, reply, &self.sync);
        }

        fn getattr(&mut self, _ino: u64, reply: fuser::ReplyAttr) {
            let attr = fuser::FileAttr {
                ino: 1,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: fuser::FileType::Directory,
                perm: 0o755,
                nlink: 1,
                uid: 501,
                gid: 20,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            };
            reply.attr(&Duration::from_secs(1), &attr);
        }
    }

    ensure_mount_directory(mount_point)?;
    let fs = DriveFs { sync };
    let mountpoint = Path::new(mount_point);
    let _session = fuser::spawn_mount2(
        fs,
        mountpoint,
        &[MountOption::AutoUnmount, MountOption::AllowRoot],
    )
    .map_err(|e| anyhow!("macFUSE mount failed: {e}"))?;

    Ok(MountHandle {
        backend: MountBackend::MacFuse,
        mount_point: mount_point.to_string(),
        webdav_port: None,
        shutdown: None,
        server_thread: None,
    })
}

#[cfg(all(windows, feature = "winfsp"))]
async fn mount_winfsp(_sync: SyncEngine, mount_point: &str) -> Result<MountHandle> {
    Err(anyhow!(
        "WinFSP mount requires driver — falling back to WebDAV for {}",
        mount_point
    ))
}

use crate::sync::SyncEngine;
use anyhow::{anyhow, Result};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

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
    child: Option<tokio::task::JoinHandle<()>>,
}

impl MountHandle {
    pub async fn unmount(self) -> Result<()> {
        if let Some(handle) = self.child {
            handle.abort();
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
    if is_mounted_at(mount_point) {
        tracing::info!("removing stale mount at {mount_point}");
        let _ = detach_webdav_mount(mount_point);
    }

    let sync_arc = Arc::new(sync);
    let server = crate::webdav::WebDavServer::new((*sync_arc).clone(), port);
    let child = tokio::spawn(async move {
        if let Err(e) = server.run().await {
            tracing::error!("webdav server stopped: {e}");
        }
    });

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    attach_webdav_mount(mount_point, port).await?;

    Ok(MountHandle {
        backend: MountBackend::WebDav,
        mount_point: mount_point.to_string(),
        webdav_port: Some(port),
        child: Some(child),
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
    let Ok(output) = Command::new("net").args(["use"]).output() else {
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
    let mount_point = mount_point.to_string();
    let url = format!("http://127.0.0.1:{port}/drive");
    ensure_mount_directory(&mount_point)?;

    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || run_mount_webdav_command(&url, &mount_point)),
    )
    .await
    .map_err(|_| anyhow!("mount timed out after 30 seconds"))?
    .map_err(|e| anyhow!("mount task failed: {e}"))??;

    Ok(())
}

#[cfg(target_os = "macos")]
fn run_mount_webdav_command(url: &str, mount_point: &str) -> Result<()> {
    let output = Command::new("/sbin/mount_webdav")
        .args([
            "-S",
            "-v",
            "Remnant Finder",
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

#[cfg(windows)]
fn run_mount_webdav_command(url: &str, mount_point: &str) -> Result<()> {
    let output = Command::new("net")
        .args(["use", mount_point, url, "/persistent:no"])
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "net use failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
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
        child: None,
    })
}

#[cfg(all(windows, feature = "winfsp"))]
async fn mount_winfsp(_sync: SyncEngine, mount_point: &str) -> Result<MountHandle> {
    Err(anyhow!(
        "WinFSP mount requires driver — falling back to WebDAV for {}",
        mount_point
    ))
}

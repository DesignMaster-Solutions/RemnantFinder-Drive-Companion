mod status;

use crate::api::DriveApiClient;
use crate::api::types::DriveNode;
use crate::cache::CacheStore;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{error, info};

pub use status::{SyncStatus, SyncStatusSnapshot};

#[derive(Clone)]
pub struct SyncEngine {
    api: Arc<DriveApiClient>,
    cache: Arc<CacheStore>,
    online: Arc<AtomicBool>,
    status: SyncStatus,
}

impl SyncEngine {
    pub fn new(api: DriveApiClient, cache: CacheStore) -> Self {
        Self {
            api: Arc::new(api),
            cache: Arc::new(cache),
            online: Arc::new(AtomicBool::new(true)),
            status: SyncStatus::default(),
        }
    }

    pub fn is_online_sync(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    pub fn status_snapshot(&self) -> SyncStatusSnapshot {
        self.status.snapshot()
    }

    pub async fn set_online(&self, online: bool) {
        self.online.store(online, Ordering::Relaxed);
    }

    pub async fn is_online(&self) -> bool {
        self.is_online_sync()
    }

    pub async fn refresh_tree(&self, path: &str) -> Result<Vec<DriveNode>> {
        let mut page = 1u32;
        let mut all = Vec::new();
        loop {
            let response = self.api.list_tree(path, page).await?;
            for node in &response.nodes {
                let json = serde_json::to_string(node)?;
                self.cache.upsert_node_json(&node.path, &json)?;
            }
            all.extend(response.nodes);
            if !response.has_more {
                break;
            }
            page += 1;
        }
        self.set_online(true).await;
        Ok(all)
    }

    pub async fn sync_changes(&self) -> Result<()> {
        if !self.is_online().await {
            return Ok(());
        }

        self.status.set_syncing("Checking for changes");
        let result = async {
            let cursor = self.cache.get_cursor()?;
            let changes = self.api.changes(cursor.as_deref(), 500).await?;
            for node in &changes.nodes {
                let json = serde_json::to_string(node)?;
                self.cache.upsert_node_json(&node.path, &json)?;
            }
            if let Some(c) = changes.cursor {
                self.cache.set_cursor(&c)?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        match result {
            Ok(()) => {
                self.set_online(true).await;
                self.status.mark_synced();
            }
            Err(e) => {
                self.set_online(false).await;
                self.status.set_error(e.to_string());
                return Err(e);
            }
        }
        Ok(())
    }

    pub async fn pin_folder(&self, path: &str) -> Result<()> {
        self.status.set_syncing(format!("Downloading {path}"));
        self.cache.pin_path(path)?;
        let result = self.download_subtree(path).await;
        match &result {
            Ok(()) => self.status.mark_synced(),
            Err(e) => self.status.set_error(e.to_string()),
        }
        result
    }

    pub fn unpin_folder(&self, path: &str) -> Result<()> {
        self.cache.unpin_path(path)
    }

    pub fn pinned_paths(&self) -> Result<Vec<String>> {
        self.cache.pinned_paths()
    }

    async fn download_subtree(&self, path: &str) -> Result<()> {
        let mut queue = vec![path.to_string()];
        while let Some(current) = queue.pop() {
            let nodes = self.refresh_tree(&current).await?;
            for node in nodes {
                if node.is_file() {
                    if let Err(e) = self.download_file_if_needed(&node).await {
                        error!("failed to cache {}: {e}", node.path);
                    }
                } else {
                    queue.push(node.path);
                }
            }
        }
        Ok(())
    }

    pub async fn download_file_if_needed(&self, node: &DriveNode) -> Result<()> {
        if !node.is_file() {
            return Ok(());
        }
        if self.cache.read_cached_file(&node.path)?.is_some() {
            return Ok(());
        }
        let bytes = self.api.download_node(node).await?;
        self.cache.write_cached_file(&node.path, &bytes)?;
        info!("cached {}", node.path);
        Ok(())
    }

    pub async fn read_file_bytes(&self, path: &str) -> Result<Vec<u8>> {
        if let Some(json) = self.cache.get_node(path)? {
            let node: DriveNode = serde_json::from_value(json)?;
            if node.is_folder() {
                anyhow::bail!("path is a directory");
            }
        }

        if let Some(cached) = self.cache.read_cached_file(path)? {
            return Ok(cached);
        }

        if !self.is_online().await {
            anyhow::bail!("offline and file not cached: {path}");
        }

        let node = self.api.resolve(path).await?;
        if node.is_folder() {
            anyhow::bail!("path is a directory");
        }
        let bytes = self.api.download_node(&node).await?;
        if self.cache.is_pinned(path)? {
            self.cache.write_cached_file(path, &bytes)?;
        }
        Ok(bytes)
    }

    pub async fn list_directory(&self, path: &str) -> Result<Vec<DriveNode>> {
        if self.is_online().await {
            match self.refresh_tree(path).await {
                Ok(nodes) => return Ok(nodes),
                Err(e) => tracing::warn!("online tree refresh failed for {path}: {e}"),
            }
        }

        self.list_directory_cached(path)
    }

    pub fn list_directory_cached(&self, path: &str) -> Result<Vec<DriveNode>> {
        Ok(self
            .cache
            .list_children(path)?
            .into_iter()
            .filter_map(|value| serde_json::from_value::<DriveNode>(value).ok())
            .collect())
    }

    pub fn cache_usage_human(&self) -> Result<String> {
        let bytes = self.cache.cache_usage_bytes()?;
        Ok(format_bytes(bytes))
    }

    pub async fn seed_root_tree(&self) -> Result<()> {
        self.refresh_tree("").await.map(|_| ())
    }

    pub async fn upload_file(
        &self,
        folder_path: &str,
        local_path: &std::path::Path,
        name: Option<&str>,
    ) -> Result<DriveNode> {
        let node = self.api.upload(folder_path, local_path, name).await?;
        let json = serde_json::to_string(&node)?;
        self.cache.upsert_node_json(&node.path, &json)?;
        Ok(node)
    }

    pub async fn mkdir(&self, parent_path: &str, name: &str) -> Result<DriveNode> {
        let node = self.api.create_folder(parent_path, name).await?;
        let json = serde_json::to_string(&node)?;
        self.cache.upsert_node_json(&node.path, &json)?;
        Ok(node)
    }

    pub async fn delete_path(&self, path: &str) -> Result<()> {
        self.api.delete_node(path).await?;
        self.cache.remove_node(path)?;
        Ok(())
    }

    pub async fn move_path(&self, from: &str, to: &str) -> Result<DriveNode> {
        let node = self.api.move_node(from, to).await?;
        self.cache.remove_node(from)?;
        let json = serde_json::to_string(&node)?;
        self.cache.upsert_node_json(&node.path, &json)?;
        Ok(node)
    }

    pub async fn run_background_loop(self, interval_secs: u64) {
        loop {
            if self.is_online().await {
                if let Err(e) = self.sync_changes().await {
                    error!("sync changes failed: {e}");
                }
                if let Ok(pinned) = self.cache.pinned_paths() {
                    for path in pinned {
                        self.status.set_syncing(format!("Updating offline copy of {path}"));
                        if let Err(e) = self.download_subtree(&path).await {
                            error!("pinned sync failed for {path}: {e}");
                            self.status.set_error(format!("Offline sync failed: {e}"));
                        } else {
                            self.status.mark_synced();
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB cached", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB cached", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB cached", bytes as f64 / KB as f64)
    } else if bytes > 0 {
        format!("{bytes} B cached")
    } else {
        "No offline cache yet".to_string()
    }
}

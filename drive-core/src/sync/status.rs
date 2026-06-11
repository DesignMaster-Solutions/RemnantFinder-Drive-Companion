use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct SyncStatus {
    inner: Arc<SyncStatusInner>,
}

#[derive(Default)]
struct SyncStatusInner {
    phase: Mutex<String>,
    detail: Mutex<String>,
    last_sync_unix: Mutex<Option<i64>>,
}

impl SyncStatus {
    pub fn set_idle(&self) {
        *self.inner.phase.lock() = "idle".to_string();
        self.inner.detail.lock().clear();
    }

    pub fn set_syncing(&self, detail: impl Into<String>) {
        *self.inner.phase.lock() = "syncing".to_string();
        *self.inner.detail.lock() = detail.into();
    }

    pub fn set_error(&self, message: impl Into<String>) {
        *self.inner.phase.lock() = "error".to_string();
        *self.inner.detail.lock() = message.into();
    }

    pub fn mark_synced(&self) {
        self.set_idle();
        *self.inner.last_sync_unix.lock() = Some(chrono::Utc::now().timestamp());
    }

    pub fn snapshot(&self) -> SyncStatusSnapshot {
        SyncStatusSnapshot {
            phase: self.inner.phase.lock().clone(),
            detail: self.inner.detail.lock().clone(),
            last_sync_unix: *self.inner.last_sync_unix.lock(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SyncStatusSnapshot {
    pub phase: String,
    pub detail: String,
    pub last_sync_unix: Option<i64>,
}

impl SyncStatusSnapshot {
    pub fn last_sync_human(&self) -> String {
        let Some(ts) = self.last_sync_unix else {
            return "Never synced yet".to_string();
        };
        let now = chrono::Utc::now().timestamp();
        let ago = (now - ts).max(0);
        if ago < 60 {
            return "Synced just now".to_string();
        }
        if ago < 3600 {
            return format!("Synced {} min ago", ago / 60);
        }
        if ago < 86400 {
            return format!("Synced {} hr ago", ago / 3600);
        }
        format!("Synced {} days ago", ago / 86400)
    }
}

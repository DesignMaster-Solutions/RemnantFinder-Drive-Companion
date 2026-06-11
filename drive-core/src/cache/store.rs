use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

pub struct CacheStore {
    conn: Mutex<Connection>,
    files_dir: PathBuf,
}

impl CacheStore {
    pub fn open(base_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_dir.join("files"))?;
        let db_path = base_dir.join("drive.db");
        let conn = Connection::open(db_path).context("open sqlite")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS nodes (
                path TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                node_type TEXT NOT NULL,
                entity_type TEXT,
                entity_id TEXT,
                etag TEXT,
                size INTEGER,
                mime_type TEXT,
                updated_at TEXT,
                json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS pinned_paths (
                path TEXT PRIMARY KEY,
                pinned_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sync_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS download_queue (
                path TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_nodes_path ON nodes(path);
            ",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            files_dir: base_dir.join("files"),
        })
    }

    pub fn upsert_node_json(&self, path: &str, json: &str) -> Result<()> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO nodes (path, name, node_type, entity_type, entity_id, etag, size, mime_type, updated_at, json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(path) DO UPDATE SET
               name=excluded.name, node_type=excluded.node_type, entity_type=excluded.entity_type,
               entity_id=excluded.entity_id, etag=excluded.etag, size=excluded.size,
               mime_type=excluded.mime_type, updated_at=excluded.updated_at, json=excluded.json",
            params![
                path,
                value.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                value.get("type").and_then(|v| v.as_str()).unwrap_or(""),
                value.get("entity_type").and_then(|v| v.as_str()),
                value.get("entity_id").and_then(|v| v.as_str()),
                value.get("etag").and_then(|v| v.as_str()),
                value.get("size").and_then(|v| v.as_i64()),
                value.get("mime_type").and_then(|v| v.as_str()),
                value.get("updated_at").and_then(|v| v.as_str()),
                json,
            ],
        )?;
        Ok(())
    }

    pub fn list_children(&self, parent_path: &str) -> Result<Vec<serde_json::Value>> {
        let prefix = if parent_path.is_empty() {
            String::new()
        } else {
            format!("{}/", parent_path.trim_end_matches('/'))
        };

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT json FROM nodes WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\' ORDER BY node_type DESC, name ASC",
        )?;

        let like_pattern = if prefix.is_empty() {
            "%".to_string()
        } else {
            format!("{}%", prefix.replace('%', "\\%"))
        };

        let rows = stmt.query_map(params![parent_path, like_pattern], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        })?;

        let mut result = Vec::new();
        for row in rows {
            let json: String = row?;
            let value: serde_json::Value = serde_json::from_str(&json)?;
            let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("");
            if parent_path.is_empty() {
                if !path.contains('/') {
                    result.push(value);
                }
            } else if path.starts_with(&prefix) {
                let rest = &path[prefix.len()..];
                if !rest.is_empty() && !rest.contains('/') {
                    result.push(value);
                }
            } else if path == parent_path {
                continue;
            }
        }
        Ok(result)
    }

    pub fn get_node(&self, path: &str) -> Result<Option<serde_json::Value>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT json FROM nodes WHERE path = ?1")?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            return Ok(Some(serde_json::from_str(&json)?));
        }
        Ok(None)
    }

    pub fn get_node_etag(&self, path: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT etag FROM nodes WHERE path = ?1")?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            return Ok(row.get(0)?);
        }
        Ok(None)
    }

    pub fn pin_path(&self, path: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO pinned_paths (path, pinned_at) VALUES (?1, datetime('now'))",
            params![path],
        )?;
        Ok(())
    }

    pub fn unpin_path(&self, path: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM pinned_paths WHERE path = ?1", params![path])?;
        Ok(())
    }

    pub fn pinned_paths(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT path FROM pinned_paths ORDER BY pinned_at")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn is_pinned(&self, path: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT 1 FROM pinned_paths WHERE ?1 = path OR ?1 LIKE path || '/%' LIMIT 1")?;
        let mut rows = stmt.query(params![path])?;
        Ok(rows.next()?.is_some())
    }

    pub fn get_cursor(&self) -> Result<Option<String>> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare("SELECT value FROM sync_state WHERE key = 'cursor'")?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            return Ok(Some(row.get(0)?));
        }
        Ok(None)
    }

    pub fn set_cursor(&self, cursor: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO sync_state (key, value) VALUES ('cursor', ?1)",
            params![cursor],
        )?;
        Ok(())
    }

    pub fn local_file_path(&self, virtual_path: &str) -> PathBuf {
        let safe = virtual_path.replace('/', "__");
        self.files_dir.join(safe)
    }

    pub fn write_cached_file(&self, virtual_path: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.local_file_path(virtual_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    pub fn read_cached_file(&self, virtual_path: &str) -> Result<Option<Vec<u8>>> {
        let path = self.local_file_path(virtual_path);
        if path.exists() {
            Ok(Some(std::fs::read(path)?))
        } else {
            Ok(None)
        }
    }

    pub fn remove_node(&self, path: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM nodes WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            params![path, format!("{}%", path.replace('%', "\\%"))],
        )?;
        drop(conn);

        let file = self.local_file_path(path);
        if file.exists() {
            let _ = std::fs::remove_file(file);
        }
        Ok(())
    }

    pub fn cache_usage_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        if self.files_dir.exists() {
            for entry in walkdir_light(&self.files_dir)? {
                if entry.path().is_file() {
                    total += entry.metadata()?.len();
                }
            }
        }
        Ok(total)
    }
}

fn walkdir_light(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut stack = vec![dir.to_path_buf()];
    let mut entries = Vec::new();
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            }
            entries.push(entry);
        }
    }
    Ok(entries)
}

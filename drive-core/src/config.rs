use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_base_url: String,
    pub company_id: Option<String>,
    pub mount_point: String,
    pub webdav_port: u16,
    pub cache_dir: Option<String>,
    #[serde(default)]
    pub auto_mount: bool,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub user_email: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_default();
        Self {
            api_base_url: "https://api.remnantfinderapp.com/api/v1".to_string(),
            company_id: None,
            mount_point: if cfg!(target_os = "windows") {
                "R:".to_string()
            } else {
                home.join("Stone Project Drive")
                    .to_string_lossy()
                    .to_string()
            },
            webdav_port: 17817,
            cache_dir: Some(home.join(".remnant-finder").to_string_lossy().to_string()),
            auto_mount: false,
            company_name: None,
            user_email: None,
        }
    }
}

impl AppConfig {
    pub fn config_file_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".remnant-finder")
            .join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_file_path();
        if !path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
                tracing::warn!("invalid config at {}: {e}", path.display());
                Self::default()
            }),
            Err(e) => {
                tracing::warn!("failed to read config at {}: {e}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create config dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self).context("serialize config")?;
        std::fs::write(&path, json).with_context(|| format!("write config {}", path.display()))?;
        Ok(())
    }

    pub fn cache_path_for_company(&self, company_id: &str) -> std::path::PathBuf {
        let base = self
            .cache_dir
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".remnant-finder"));
        base.join(company_id)
    }
}

use crate::api::types::{ApiEnvelope, ChangesResponse, DriveNode, TreeResponse};
use anyhow::{anyhow, Context, Result};
use reqwest::Response;
use reqwest::Client;
use std::path::Path;
use std::time::Duration;

pub struct DriveApiClient {
    http: Client,
    base_url: String,
    token: String,
    company_id: String,
}

impl DriveApiClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>, company_id: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .user_agent("StoneProject-Drive/1.0")
                .timeout(Duration::from_secs(20))
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("http client"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            company_id: company_id.into(),
        }
    }

    fn drive_url(&self, path: &str) -> String {
        format!(
            "{}/companies/{}/drive/{}",
            self.base_url, self.company_id, path
        )
    }

    async fn check_envelope<T>(&self, response: Response) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let status = response.status();
        let body = response.text().await.context("read API response")?;
        let envelope: ApiEnvelope<T> = serde_json::from_str(&body)
            .map_err(|_| anyhow!("API returned invalid JSON ({status}): {}", body.chars().take(200).collect::<String>()))?;
        if !envelope.success {
            return Err(anyhow!(
                envelope.message.unwrap_or_else(|| "API error".into())
            ));
        }
        envelope
            .data
            .ok_or_else(|| anyhow!("missing data (status {status})"))
    }

    pub async fn login(base_url: &str, email: &str, password: &str) -> Result<(String, serde_json::Value)> {
        let client = Client::builder()
            .user_agent("StoneProject-Drive/1.0")
            .build()
            .context("build HTTP client")?;
        let url = format!("{}/auth/login", base_url.trim_end_matches('/'));
        let response = client
            .post(&url)
            .header("X-Client", "companion-drive")
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
            .context("network error contacting API")?;

        let status = response.status();
        let body = response.text().await.context("read login response")?;

        if let Ok(envelope) = serde_json::from_str::<ApiEnvelope<serde_json::Value>>(&body) {
            if !envelope.success {
                return Err(anyhow!(
                    envelope.message.unwrap_or_else(|| "login failed".into())
                ));
            }
            let data = envelope.data.ok_or_else(|| anyhow!("missing login data"))?;
            let token = data
                .get("token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("missing token"))?
                .to_string();
            return Ok((token, data));
        }

        if !status.is_success() {
            return Err(anyhow!("login failed ({status}): {body}"));
        }

        Err(anyhow!("unexpected login response: {body}"))
    }

    pub fn company_id_from_login(data: &serde_json::Value) -> Result<String> {
        if let Some(id) = data
            .get("user")
            .and_then(|user| user.get("company"))
            .and_then(|company| company.get("id"))
            .and_then(|id| id.as_str())
        {
            return Ok(id.to_string());
        }

        if let Some(id) = data
            .get("user")
            .and_then(|user| user.get("current_company_id"))
            .and_then(|id| id.as_str())
        {
            return Ok(id.to_string());
        }

        Err(anyhow!(
            "no company linked to this account — use the main app to join or create a company first"
        ))
    }

    pub async fn list_tree(&self, path: &str, page: u32) -> Result<TreeResponse> {
        let response = self
            .http
            .get(self.drive_url("tree"))
            .bearer_auth(&self.token)
            .query(&[("path", path), ("page", &page.to_string())])
            .send()
            .await?;
        self.check_envelope(response).await
    }

    pub async fn resolve(&self, path: &str) -> Result<DriveNode> {
        let response = self
            .http
            .get(self.drive_url("resolve"))
            .bearer_auth(&self.token)
            .query(&[("path", path)])
            .send()
            .await?;
        self.check_envelope(response).await
    }

    pub async fn changes(&self, cursor: Option<&str>, limit: u32) -> Result<ChangesResponse> {
        let mut query: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = cursor {
            query.push(("cursor", c.to_string()));
        }
        let response = self
            .http
            .get(self.drive_url("changes"))
            .bearer_auth(&self.token)
            .query(&query)
            .send()
            .await?;
        self.check_envelope(response).await
    }

    pub async fn download_project_file(&self, file_id: &str) -> Result<Vec<u8>> {
        let url = self.drive_url(&format!("files/{file_id}/content"));
        let response = self.http.get(url).bearer_auth(&self.token).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!("download failed: {}", response.status()));
        }
        Ok(response.bytes().await?.to_vec())
    }

    pub async fn download_media(&self, media_id: &str) -> Result<Vec<u8>> {
        let url = self.drive_url(&format!("media/{media_id}/content"));
        let response = self.http.get(url).bearer_auth(&self.token).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!("download failed: {}", response.status()));
        }
        Ok(response.bytes().await?.to_vec())
    }

    pub async fn download_node(&self, node: &DriveNode) -> Result<Vec<u8>> {
        match node.entity_type.as_deref() {
            Some("project_file") => {
                let id = node.entity_id.as_deref().ok_or_else(|| anyhow!("missing file id"))?;
                self.download_project_file(id).await
            }
            Some("company_media") => {
                let id = node.entity_id.as_deref().ok_or_else(|| anyhow!("missing media id"))?;
                self.download_media(id).await
            }
            _ => Err(anyhow!("unsupported entity type")),
        }
    }

    pub async fn upload(&self, folder_path: &str, local_path: &Path, name: Option<&str>) -> Result<DriveNode> {
        let file_name = name
            .map(String::from)
            .unwrap_or_else(|| {
                local_path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "upload.bin".into())
            });

        let part = reqwest::multipart::Part::file(local_path)
            .await?
            .file_name(file_name);
        let form = reqwest::multipart::Form::new()
            .text("path", folder_path.to_string())
            .part("file", part);

        let response = self
            .http
            .post(self.drive_url("upload"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await?;
        self.check_envelope(response).await
    }

    pub async fn create_folder(&self, parent_path: &str, name: &str) -> Result<DriveNode> {
        let response = self
            .http
            .post(self.drive_url("folders"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "path": parent_path, "name": name }))
            .send()
            .await?;
        self.check_envelope(response).await
    }

    pub async fn move_node(&self, from_path: &str, to_path: &str) -> Result<DriveNode> {
        let response = self
            .http
            .patch(self.drive_url("nodes"))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "from_path": from_path, "to_path": to_path }))
            .send()
            .await?;
        self.check_envelope(response).await
    }

    pub async fn delete_node(&self, path: &str) -> Result<()> {
        let response = self
            .http
            .delete(self.drive_url("nodes"))
            .bearer_auth(&self.token)
            .query(&[("path", path)])
            .send()
            .await?;
        let envelope: ApiEnvelope<serde_json::Value> = response.json().await?;
        if !envelope.success {
            return Err(anyhow!(envelope.message.unwrap_or_else(|| "delete failed".into())));
        }
        Ok(())
    }
}

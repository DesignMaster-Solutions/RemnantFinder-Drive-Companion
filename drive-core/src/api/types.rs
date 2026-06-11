use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriveNode {
    pub name: String,
    pub path: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub scope: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<i64>,
    pub etag: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub success: bool,
    pub message: Option<String>,
    pub data: Option<T>,
}

#[derive(Debug, Deserialize)]
pub struct TreeResponse {
    pub path: String,
    pub nodes: Vec<DriveNode>,
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct ChangesResponse {
    pub nodes: Vec<DriveNode>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

impl DriveNode {
    pub fn is_folder(&self) -> bool {
        self.node_type == "folder"
    }

    pub fn is_file(&self) -> bool {
        self.node_type == "file"
    }
}

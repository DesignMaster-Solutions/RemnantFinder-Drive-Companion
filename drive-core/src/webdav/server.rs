use crate::api::types::DriveNode;
use crate::sync::SyncEngine;
use anyhow::{anyhow, Result};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 512 * 1024 * 1024;

pub struct WebDavServer {
    sync: SyncEngine,
    port: u16,
}

impl WebDavServer {
    pub fn new(sync: SyncEngine, port: u16) -> Self {
        Self { sync, port }
    }

    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(("127.0.0.1", self.port)).await?;
        tracing::info!("WebDAV server listening on 127.0.0.1:{}", self.port);
        let sync = Arc::new(self.sync);
        loop {
            let (mut socket, _) = listener.accept().await?;
            let sync = sync.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(&mut socket, sync).await {
                    tracing::debug!("connection closed: {e}");
                }
            });
        }
    }
}

async fn handle_connection(socket: &mut tokio::net::TcpStream, sync: Arc<SyncEngine>) -> Result<()> {
    let request_bytes = match read_http_request(socket, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("failed to read request: {e}");
            write_response(socket, 400, "Bad Request", "", &[]).await?;
            return Ok(());
        }
    };

    if let Err(e) = process_request(socket, sync, &request_bytes).await {
        tracing::warn!("request failed: {e}");
        write_response(socket, 500, "Internal Server Error", "", &[]).await?;
    }
    Ok(())
}

async fn read_http_request(socket: &mut tokio::net::TcpStream, max_body: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];

    loop {
        let n = socket.read(&mut buf).await?;
        if n == 0 {
            return Err(anyhow!("unexpected EOF while reading headers"));
        }
        data.extend_from_slice(&buf[..n]);

        if data.len() > MAX_HEADER_BYTES && find_header_end(&data).is_none() {
            return Err(anyhow!("request headers too large"));
        }

        if let Some(end) = find_header_end(&data) {
            let header_len = end + 4;
            let headers = String::from_utf8_lossy(&data[..header_len]);
            let content_length = parse_content_length(&headers).unwrap_or(0);
            if content_length > max_body {
                return Err(anyhow!("request body exceeds limit"));
            }
            let total = header_len + content_length;
            while data.len() < total {
                let n = socket.read(&mut buf).await?;
                if n == 0 {
                    return Err(anyhow!("unexpected EOF while reading body"));
                }
                data.extend_from_slice(&buf[..n]);
            }
            data.truncate(total);
            return Ok(data);
        }
    }
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(headers: &str) -> Option<usize> {
    parse_header(headers, "Content-Length")?
        .parse()
        .ok()
}

fn parse_header(headers: &str, name: &str) -> Option<String> {
    for line in headers.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case(name) {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

async fn process_request(
    socket: &mut tokio::net::TcpStream,
    sync: Arc<SyncEngine>,
    request_bytes: &[u8],
) -> Result<()> {
    let header_end = find_header_end(request_bytes)
        .ok_or_else(|| anyhow!("missing HTTP header terminator"))?
        + 4;
    let request = String::from_utf8_lossy(&request_bytes[..header_end]);
    let body = &request_bytes[header_end..];

    let mut lines = request.lines();
    let request_line = lines.next().ok_or_else(|| anyhow!("empty request"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_path = parts.next().unwrap_or("/");
    let path = urlencoding::decode(raw_path.trim_start_matches("/drive"))
        .unwrap_or_default()
        .into_owned();
    let virtual_path = path.trim_start_matches('/').to_string();

    match method {
        "OPTIONS" => {
            write_response(
                socket,
                200,
                "OK",
                "DAV: 1, 2\r\nAllow: GET, HEAD, PUT, DELETE, MKCOL, PROPFIND, MOVE, LOCK, UNLOCK, OPTIONS",
                &[],
            )
            .await
        }
        "PROPFIND" => {
            let nodes = match sync
                .list_directory(if virtual_path.is_empty() {
                    ""
                } else {
                    &virtual_path
                })
                .await
            {
                Ok(nodes) => nodes,
                Err(e) => {
                    tracing::warn!("PROPFIND failed for {virtual_path:?}: {e}");
                    Vec::new()
                }
            };
            let body = propfind_xml(&virtual_path, &nodes);
            write_response(
                socket,
                207,
                "Multi-Status",
                "Content-Type: application/xml; charset=utf-8",
                body.as_bytes(),
            )
            .await
        }
        "GET" | "HEAD" => {
            if virtual_path.is_empty() {
                return write_response(socket, 200, "OK", "", &[]).await;
            }
            match sync.read_file_bytes(&virtual_path).await {
                Ok(bytes) => {
                    let len_header = format!("Content-Length: {}", bytes.len());
                    if method == "HEAD" {
                        write_response(socket, 200, "OK", &len_header, &[]).await
                    } else {
                        write_response(
                            socket,
                            200,
                            "OK",
                            "Content-Type: application/octet-stream",
                            &bytes,
                        )
                        .await
                    }
                }
                Err(e) => {
                    tracing::debug!("GET/HEAD {virtual_path}: {e}");
                    write_response(socket, 404, "Not Found", "", &[]).await
                }
            }
        }
        "PUT" => {
            let parent = parent_path(&virtual_path);
            let file_name = file_name_from_path(&virtual_path);
            let tmp = std::env::temp_dir().join(format!("rf-upload-{}", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, body).await?;
            let response = match sync
                .upload_file(&parent, &tmp, Some(&file_name))
                .await
            {
                Ok(_) => write_response(socket, 201, "Created", "", &[]).await,
                Err(e) => {
                    tracing::error!("upload failed: {e}");
                    write_response(socket, 500, "Internal Server Error", "", &[]).await
                }
            };
            let _ = tokio::fs::remove_file(tmp).await;
            response
        }
        "MKCOL" => {
            let parent = parent_path(&virtual_path);
            let name = file_name_from_path(&virtual_path);
            match sync.mkdir(&parent, &name).await {
                Ok(_) => write_response(socket, 201, "Created", "", &[]).await,
                Err(e) => {
                    tracing::error!("mkdir failed: {e}");
                    write_response(socket, 500, "Internal Server Error", "", &[]).await
                }
            }
        }
        "DELETE" => match sync.delete_path(&virtual_path).await {
            Ok(_) => write_response(socket, 204, "No Content", "", &[]).await,
            Err(e) => {
                tracing::error!("delete failed: {e}");
                write_response(socket, 500, "Internal Server Error", "", &[]).await
            }
        },
        "MOVE" => {
            let destination = parse_header(&request, "Destination")
                .ok_or_else(|| anyhow!("MOVE missing Destination header"))?;
            let dest_path = href_to_virtual_path(&destination);
            if dest_path.is_empty() {
                return write_response(socket, 400, "Bad Request", "", &[]).await;
            }
            match sync.move_path(&virtual_path, &dest_path).await {
                Ok(_) => write_response(socket, 201, "Created", "", &[]).await,
                Err(e) => {
                    tracing::error!("move failed: {e}");
                    write_response(socket, 500, "Internal Server Error", "", &[]).await
                }
            }
        }
        "LOCK" => {
            write_response(
                socket,
                200,
                "OK",
                "Content-Type: text/xml; charset=utf-8\r\nLock-Token: <opaquelocktoken:remnant-finder>",
                lock_discovery_xml(raw_path).as_bytes(),
            )
            .await
        }
        "UNLOCK" => write_response(socket, 204, "No Content", "", &[]).await,
        _ => write_response(socket, 405, "Method Not Allowed", "", &[]).await,
    }
}

fn href_to_virtual_path(href: &str) -> String {
    let decoded = urlencoding::decode(href)
        .unwrap_or_else(|_| href.into())
        .into_owned();

    if let Some(idx) = decoded.find("/drive/") {
        return decoded[idx + 7..].trim_start_matches('/').to_string();
    }
    if let Some(idx) = decoded.rfind("/drive") {
        let rest = &decoded[idx + 6..];
        return rest.trim_start_matches('/').to_string();
    }

    decoded.trim_start_matches('/').to_string()
}

async fn write_response(
    socket: &mut tokio::net::TcpStream,
    code: u16,
    status: &str,
    extra_headers: &str,
    body: &[u8],
) -> Result<()> {
    let mut header_block = format!("HTTP/1.1 {code} {status}\r\n");
    if !extra_headers.is_empty() {
        header_block.push_str(extra_headers);
        header_block.push_str("\r\n");
    }
    header_block.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    socket.write_all(header_block.as_bytes()).await?;
    if !body.is_empty() {
        socket.write_all(body).await?;
    }
    Ok(())
}

fn lock_discovery_xml(path: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
         <D:prop xmlns:D=\"DAV:\"><D:lockdiscovery><D:activelock>\
         <D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope>\
         <D:depth>infinity</D:depth><D:timeout>Second-3600</D:timeout>\
         <D:locktoken><D:href>opaquelocktoken:remnant-finder</D:href></D:locktoken>\
         <D:lockroot><D:href>{path}</D:href></D:lockroot></D:activelock></D:lockdiscovery></D:prop>"
    )
}

fn propfind_xml(parent_path: &str, nodes: &[DriveNode]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?><D:multistatus xmlns:D=\"DAV:\">",
    );
    if parent_path.is_empty() {
        xml.push_str(&prop_entry("", true, 0));
    } else {
        xml.push_str(&prop_entry(parent_path, true, 0));
    }
    for node in nodes {
        xml.push_str(&prop_entry(
            &node.path,
            node.is_folder(),
            node.size.unwrap_or(0) as u64,
        ));
    }
    xml.push_str("</D:multistatus>");
    xml
}

fn prop_entry(path: &str, is_collection: bool, size: u64) -> String {
    let href = if path.is_empty() {
        "/drive/".to_string()
    } else {
        format!("/drive/{}", path.replace(' ', "%20"))
    };
    let resourcetype = if is_collection {
        "<D:resourcetype><D:collection/></D:resourcetype>"
    } else {
        "<D:resourcetype/>"
    };
    let display_name = if path.is_empty() {
        "drive".to_string()
    } else {
        file_name_from_path(path)
    };
    format!(
        "<D:response><D:href>{href}</D:href><D:propstat><D:prop>\
         <D:displayname>{}</D:displayname>{resourcetype}<D:getcontentlength>{size}</D:getcontentlength>\
         </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
        xml_escape(&display_name)
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .unwrap_or_default()
}

fn file_name_from_path(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_destination_href() {
        assert_eq!(
            href_to_virtual_path("http://127.0.0.1:17817/drive/Projects/Client/file.pdf"),
            "Projects/Client/file.pdf"
        );
        assert_eq!(
            href_to_virtual_path("/drive/Accounts/Acme"),
            "Accounts/Acme"
        );
    }

    #[test]
    fn parses_content_length() {
        let headers = "PUT /drive/x HTTP/1.1\r\nContent-Length: 42\r\n\r\n";
        assert_eq!(parse_content_length(headers), Some(42));
    }
}

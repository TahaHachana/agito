//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A `data:` URL for an HTML snippet.
pub fn html(body: &str) -> String {
    let mut encoded = String::new();
    for b in format!("<!doctype html><html><head><meta charset=utf-8><title>t</title></head><body>{body}</body></html>").bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("data:text/html,{encoded}")
}

/// A tiny HTTP server: path -> (status, body). Returns its base URL.
///
/// Conventions for network tests:
/// - a body starting `REDIRECT:` answers with a `Location` header pointing at the rest;
/// - a body `ATTACHMENT:<name>:<body>` is served as a download named `<name>`;
/// - a path containing `/slow` answers after 400ms;
/// - `/echo` answers with JSON describing the request (method, path, body);
/// - a body starting `{` or `[` is served as JSON, anything else as HTML.
pub async fn serve(routes: &[(&str, u16, &str)]) -> String {
    let routes: Arc<HashMap<String, (u16, String)>> = Arc::new(
        routes
            .iter()
            .map(|(p, s, b)| (p.to_string(), (*s, b.to_string())))
            .collect(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            tokio::spawn(async move {
                let request = read_request(&mut socket).await;
                let mut lines = request.split("\r\n");
                let first = lines.next().unwrap_or_default().to_owned();
                let method = first.split_whitespace().next().unwrap_or("GET").to_owned();
                let full_path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let path = full_path.split('?').next().unwrap_or("/").to_owned();
                let body = request
                    .split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_owned())
                    .unwrap_or_default();
                if path.contains("/slow") {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
                let (status, content) = if path == "/echo" {
                    (
                        200,
                        format!(
                            r#"{{"method":"{method}","path":"{full_path}","body":{}}}"#,
                            serde_json_string(&body)
                        ),
                    )
                } else {
                    routes
                        .get(&path)
                        .cloned()
                        .unwrap_or((404, "<h1>not found</h1>".into()))
                };
                let mut extra = String::new();
                let content = if let Some(location) = content.strip_prefix("REDIRECT:") {
                    extra.push_str(&format!("Location: {location}\r\n"));
                    String::new()
                } else if let Some(rest) = content.strip_prefix("ATTACHMENT:") {
                    // ATTACHMENT:<file name>:<body> is served as a download.
                    let (name, body) = rest.split_once(':').unwrap_or((rest, ""));
                    extra.push_str(&format!(
                        "Content-Disposition: attachment; filename=\"{name}\"\r\n"
                    ));
                    body.to_owned()
                } else {
                    content
                };
                let content_type = if !extra.is_empty() && extra.starts_with("Content-Disposition")
                {
                    "text/csv"
                } else if content.starts_with('{') || content.starts_with('[') {
                    "application/json"
                } else {
                    "text/html; charset=utf-8"
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Agito: yes\r\nAccess-Control-Allow-Origin: *\r\n{extra}Connection: close\r\n\r\n{content}",
                    content.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Reads one request, including a `Content-Length` body.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = socket.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let length = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            if body.len() >= length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn serde_json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

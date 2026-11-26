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

/// A tiny HTTP server: path -> (status, html). Returns its base URL.
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
                let mut buf = vec![0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_owned();
                let (status, body) = routes
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, "<h1>not found</h1>".into()));
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nX-Agito: yes\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{port}")
}

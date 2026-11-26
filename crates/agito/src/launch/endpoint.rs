//! Discovering a browser's WebSocket endpoint from its HTTP debugging port (`/json/version`).

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{Error, ErrorKind, Result};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);

/// Polls `http://host:port/json/version` until it names a `webSocketDebuggerUrl`.
pub async fn wait_for_ws_endpoint(host: &str, port: u16, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = String::new();
    while Instant::now() < deadline {
        match tokio::time::timeout(REQUEST_TIMEOUT, get(host, port, "/json/version")).await {
            Ok(Ok(body)) => match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(json) => match json["webSocketDebuggerUrl"].as_str() {
                    Some(url) => return Ok(url.to_owned()),
                    None => last_error = "it answered without a webSocketDebuggerUrl".into(),
                },
                Err(e) => last_error = format!("it answered with something other than JSON: {e}"),
            },
            Ok(Err(e)) => last_error = e.to_string(),
            Err(_) => last_error = "the request timed out".into(),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Err(ErrorKind::WaitTimeout {
        condition: "browser debugging endpoint".into(),
        timeout,
        last_evaluated_value: Some(last_error.clone()),
        message: format!(
            "Browser did not provide a CDP endpoint within {}s at http://{host}:{port}. The last attempt to reach it said: {last_error}",
            timeout.as_secs_f64()
        ),
    }
    .into())
}

/// Resolves what a user passed to `connect`: a `ws://` URL is used as-is; an `http://` URL is
/// asked for its WebSocket endpoint.
pub async fn resolve_endpoint(endpoint: &str, timeout: Duration) -> Result<String> {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_owned());
    }
    let rest = endpoint
        .strip_prefix("http://")
        .ok_or_else(|| {
            Error::invalid_argument(format!(
                "'{endpoint}' is not a valid browser endpoint. Expected a CDP WebSocket URL or an HTTP debugging endpoint such as http://127.0.0.1:9222."
            ))
        })?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| Error::invalid_argument(format!("bad port in '{endpoint}'")))?,
        ),
        None => (authority, 80),
    };
    wait_for_ws_endpoint(host, port, timeout).await
}

/// A minimal HTTP/1.1 GET returning the body; enough for the DevTools JSON endpoints.
async fn get(host: &str, port: u16, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect((host, port)).await?;
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    let text = String::from_utf8_lossy(&response);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| std::io::Error::other("malformed HTTP response"))?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(std::io::Error::other(
            head.lines().next().unwrap_or("bad status").to_owned(),
        ));
    }
    Ok(body.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn reads_ws_url_from_json_version() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await.unwrap();
            let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let url = wait_for_ws_endpoint("127.0.0.1", port, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(url, "ws://127.0.0.1:1/devtools/browser/x");
    }

    #[tokio::test]
    async fn times_out_with_reason() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let err = wait_for_ws_endpoint("127.0.0.1", port, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.is_timeout());
        assert!(err.to_string().contains("did not provide a CDP endpoint"));
    }

    #[tokio::test]
    async fn ws_endpoints_pass_through_and_bad_schemes_fail() {
        assert_eq!(
            resolve_endpoint("ws://h:1/x", Duration::ZERO)
                .await
                .unwrap(),
            "ws://h:1/x"
        );
        assert!(resolve_endpoint("ftp://h", Duration::ZERO).await.is_err());
    }
}

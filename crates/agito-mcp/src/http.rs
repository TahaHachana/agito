//! Serving MCP over Streamable HTTP, for remote clients.
//!
//! Each MCP session gets its own server and so its own browser, torn down when the session
//! ends or after 30 minutes without a request. The server binds the loopback interface unless
//! told otherwise, and refuses to bind any other without a bearer token, which every request
//! must then carry.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::{AgitoServer, ServerOptions};

/// How long a session may go without a request before it and its browser are torn down.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Where and how `agito mcp --http` listens.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// The interface to bind. Default: 127.0.0.1.
    pub host: IpAddr,
    /// The TCP port. 0 picks a free one.
    pub port: u16,
    /// The bearer token every request must carry. Required off loopback.
    pub token: Option<String>,
}

impl Default for HttpOptions {
    fn default() -> Self {
        HttpOptions {
            host: IpAddr::from([127, 0, 0, 1]),
            port: 8931,
            token: None,
        }
    }
}

/// Checks the options before binding: a server reachable from other machines needs a token.
pub fn check(options: &HttpOptions) -> Result<(), String> {
    if !options.host.is_loopback() && options.token.as_deref().is_none_or(str::is_empty) {
        return Err(format!(
            "binding {} makes the server reachable from other machines, so it needs a token: \
             pass --token or set AGITO_MCP_TOKEN",
            options.host
        ));
    }
    Ok(())
}

/// Serves until `stop` resolves, calling `ready` with the address bound once it listens.
pub async fn serve_http(
    server_options: ServerOptions,
    options: HttpOptions,
    ready: impl FnOnce(SocketAddr),
    stop: impl Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    check(&options)?;
    let server_options = Arc::new(server_options);
    let factory = move || Ok(AgitoServer::new(server_options.clone()).for_http());
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(IDLE_TIMEOUT);
    let mut config = StreamableHttpServerConfig::default();
    // Off loopback, the Host a client sends is not known in advance; the token stands in for
    // the DNS-rebinding defence the loopback check gives a local server.
    if !options.host.is_loopback() {
        config = config.disable_allowed_hosts();
    }
    let cancel = config.cancellation_token.clone();
    let service = StreamableHttpService::new(factory, Arc::new(sessions), config);

    let mut app = axum::Router::new().nest_service("/mcp", service);
    if let Some(token) = options.token.clone().filter(|t| !t.is_empty()) {
        let token = Arc::new(token);
        app = app.layer(middleware::from_fn(move |request, next| {
            let token = token.clone();
            async move { require_token(&token, request, next).await }
        }));
    }
    let listener = tokio::net::TcpListener::bind((options.host, options.port)).await?;
    ready(listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            stop.await;
            cancel.cancel();
        })
        .await?;
    Ok(())
}

async fn require_token(token: &str, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if constant_time_eq(presented.as_bytes(), token.as_bytes()) {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "a valid bearer token is required",
        )
            .into_response()
    }
}

/// Compares in time that depends only on the lengths, so a wrong token reveals nothing about
/// how much of it was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_loopback_a_token_is_required() {
        let local = HttpOptions::default();
        assert!(check(&local).is_ok());
        let open = HttpOptions {
            host: IpAddr::from([0, 0, 0, 0]),
            ..HttpOptions::default()
        };
        assert!(check(&open).unwrap_err().contains("needs a token"));
        let empty = HttpOptions {
            token: Some(String::new()),
            ..open.clone()
        };
        assert!(check(&empty).is_err());
        let guarded = HttpOptions {
            token: Some("s3cret".into()),
            ..open
        };
        assert!(check(&guarded).is_ok());
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}

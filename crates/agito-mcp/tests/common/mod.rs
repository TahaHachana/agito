//! An MCP client connected to a fresh server, in process, and a small HTTP server.
#![allow(dead_code)]

use agito_mcp::ServerOptions;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use serde_json::Value;
use tokio::task::JoinHandle;

/// A client connected to a fresh server, and the server's task.
pub struct Harness {
    pub client: RunningService<RoleClient, ()>,
    server: JoinHandle<()>,
}

impl Harness {
    pub async fn start() -> Harness {
        Harness::with(ServerOptions::default()).await
    }

    pub async fn with(options: ServerOptions) -> Harness {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            let (reader, writer) = tokio::io::split(server_io);
            agito_mcp::serve(options, reader, writer)
                .await
                .expect("the server runs until the client leaves");
        });
        let client = ().serve(client_io).await.expect("the client connects");
        Harness { client, server }
    }

    pub async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let Value::Object(args) = args else {
            panic!("arguments are an object")
        };
        self.client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
            .await
            .unwrap_or_else(|e| panic!("{tool} reached the server: {e}"))
    }

    /// Calls `tool`, which must succeed, and returns its text.
    pub async fn ok(&self, tool: &str, args: Value) -> String {
        let result = self.call(tool, args).await;
        let text = text_of(&result);
        assert_ne!(result.is_error, Some(true), "{tool} failed: {text}");
        text
    }

    /// Calls `tool`, which must fail, and returns its message.
    pub async fn err(&self, tool: &str, args: Value) -> String {
        let result = self.call(tool, args).await;
        let text = text_of(&result);
        assert_eq!(result.is_error, Some(true), "{tool} succeeded: {text}");
        text
    }

    /// Disconnects and waits for the server to close its browser.
    pub async fn stop(self) {
        self.client.cancel().await.expect("the client disconnects");
        self.server.await.expect("the server shuts down cleanly");
    }
}

pub fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

pub fn page(body: &str) -> String {
    format!("data:text/html,<html lang=en><title>Shop</title>{body}</html>")
}

/// A server that answers every request on 127.0.0.1 with the response `route` gives for its
/// method and path: status, content type and body. Returns its origin.
pub async fn http_server(route: fn(&str, &str) -> (u16, &'static str, String)) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0; 16 * 1024];
                let Ok(n) = socket.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..n]).into_owned();
                let mut first = request.lines().next().unwrap_or_default().split(' ');
                let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or("/"));
                let (status, content_type, body) = route(method, path);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    origin
}

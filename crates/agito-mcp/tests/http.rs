//! `agito mcp --http`: Streamable HTTP, a browser per session, and the bearer token. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

use std::net::SocketAddr;

use agito_mcp::{HttpOptions, ServerOptions};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use tokio::sync::oneshot;

/// Starts a server on a free loopback port; dropping the sender stops it.
async fn start(token: Option<&str>) -> (SocketAddr, oneshot::Sender<()>) {
    let (address_tx, address_rx) = oneshot::channel();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let options = HttpOptions {
        port: 0,
        token: token.map(str::to_owned),
        ..HttpOptions::default()
    };
    tokio::spawn(async move {
        agito_mcp::serve_http(
            ServerOptions::default(),
            options,
            |address| {
                let _ = address_tx.send(address);
            },
            async move {
                let _ = stop_rx.await;
            },
        )
        .await
        .expect("the server runs until stopped");
    });
    (address_rx.await.unwrap(), stop_tx)
}

async fn connect(address: SocketAddr, token: Option<&str>) -> RunningService<RoleClient, ()> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(format!("http://{address}/mcp"));
    if let Some(token) = token {
        config = config.auth_header(token);
    }
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .expect("the client connects")
}

async fn call(client: &RunningService<RoleClient, ()>, tool: &str, args: Value) -> String {
    let Value::Object(args) = args else { panic!() };
    let result = client
        .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{tool}: {result:?}");
    result.content[0].as_text().unwrap().text.clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_client_session_drives_a_browser_of_its_own() {
    let (address, _stop) = start(None).await;
    let first = connect(address, None).await;
    let second = connect(address, None).await;
    // Sessions exist only up to 2025-11-25, and the browser lives in the session.
    let negotiated = first.peer_info().unwrap().protocol_version.clone();
    assert!(
        negotiated <= ProtocolVersion::LATEST_WITH_INITIALIZE,
        "{negotiated:?}"
    );

    call(
        &first,
        "navigate",
        json!({ "url": "data:text/html,<title>First</title>" }),
    )
    .await;
    // The browser outlives the request: the next call sees the same page.
    assert!(
        call(&first, "tab_list", json!({}))
            .await
            .ends_with(" | First")
    );
    assert!(
        call(&second, "browser_status", json!({}))
            .await
            .starts_with("No browser yet")
    );
    call(
        &second,
        "navigate",
        json!({ "url": "data:text/html,<title>Second</title>" }),
    )
    .await;
    assert_eq!(call(&first, "tab_list", json!({})).await.lines().count(), 1);
    assert!(
        call(&second, "tab_list", json!({}))
            .await
            .ends_with(" | Second")
    );
    first.cancel().await.unwrap();
    second.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_guards_every_request() {
    let (address, _stop) = start(Some("s3cret")).await;
    let refused = reqwest::Client::new()
        .post(format!("http://{address}/mcp"))
        .header("content-type", "application/json")
        .body(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    let wrong = reqwest::Client::new()
        .post(format!("http://{address}/mcp"))
        .bearer_auth("guess")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    let client = connect(address, Some("s3cret")).await;
    assert!(
        call(&client, "browser_status", json!({}))
            .await
            .starts_with("No browser yet")
    );
    client.cancel().await.unwrap();
}

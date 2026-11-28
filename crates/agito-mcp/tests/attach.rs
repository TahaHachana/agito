//! Driving a browser that is already running: `--connect`, and `browser_attach`. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

mod common;

use agito_mcp::ServerOptions;
use common::{Harness, RunningChrome};
use serde_json::json;

const MINE: &str = "data:text/html,<title>Mine</title><input id=q value=draft>";

#[tokio::test(flavor = "multi_thread")]
async fn connect_drives_what_is_open_and_leaves_it_running() {
    let chrome = RunningChrome::start("connect", MINE).await;
    let h = Harness::with(ServerOptions {
        connect: Some(chrome.endpoint.clone()),
        ..ServerOptions::default()
    })
    .await;
    let status = h.ok("browser_status", json!({})).await;
    assert!(
        status.starts_with("No browser yet. The first tool call that needs one will attach to"),
        "{status}"
    );
    // The tab that was already open is the active one, with what the person typed in it.
    let tree = h.ok("snapshot", json!({})).await;
    assert!(tree.starts_with("- RootWebArea \"Mine\""), "{tree}");
    assert!(tree.contains(r#"[value="draft"]"#), "{tree}");
    h.ok("type", json!({ "ref": "#q", "text": "agent" })).await;
    h.ok(
        "tab_open",
        json!({ "url": "data:text/html,<title>Agent tab</title>" }),
    )
    .await;
    let status = h.ok("browser_status", json!({})).await;
    assert!(
        status.starts_with("Attached to a running browser at"),
        "{status}"
    );
    assert!(
        status.ends_with("1 context (active: default), 2 tabs open."),
        "{status}"
    );
    h.stop().await;

    // The browser is still running, with both tabs.
    assert_eq!(chrome.titles().await, ["Agent tab", "Mine"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_attach_is_listed_only_with_leave_to_attach() {
    let plain = Harness::start().await;
    let tools = plain.client.list_all_tools().await.unwrap();
    assert!(!tools.iter().any(|t| t.name == "browser_attach"));
    plain.stop().await;

    let chrome = RunningChrome::start("attach", MINE).await;
    let h = Harness::with(ServerOptions {
        allow_attach: true,
        ..ServerOptions::default()
    })
    .await;
    // First a browser of the server's own.
    h.ok(
        "navigate",
        json!({ "url": "data:text/html,<title>Launched</title>" }),
    )
    .await;
    let attached = h
        .ok("browser_attach", json!({ "endpoint": chrome.endpoint }))
        .await;
    assert!(
        attached.ends_with("1 tab is open. Take a snapshot before addressing elements."),
        "{attached}"
    );
    let tabs = h.ok("tab_list", json!({})).await;
    assert!(tabs.ends_with(" | Mine"), "{tabs}");
    assert_eq!(
        tabs.lines().count(),
        1,
        "the launched browser's tab is gone: {tabs}"
    );
    h.stop().await;
    assert_eq!(chrome.titles().await, ["Mine"]);
}

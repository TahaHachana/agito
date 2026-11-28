//! The MCP server driven by an MCP client, in process, against real pages. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

use agito_mcp::ServerOptions;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

/// A client connected to a fresh server, and the server's task.
struct Harness {
    client: RunningService<RoleClient, ()>,
    server: JoinHandle<()>,
}

impl Harness {
    async fn start() -> Harness {
        Harness::with(ServerOptions::default()).await
    }

    async fn with(options: ServerOptions) -> Harness {
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

    async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let Value::Object(args) = args else {
            panic!("arguments are an object")
        };
        self.client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
            .await
            .unwrap_or_else(|e| panic!("{tool} reached the server: {e}"))
    }

    /// Calls `tool`, which must succeed, and returns its text.
    async fn ok(&self, tool: &str, args: Value) -> String {
        let result = self.call(tool, args).await;
        let text = text_of(&result);
        assert_ne!(result.is_error, Some(true), "{tool} failed: {text}");
        text
    }

    /// Calls `tool`, which must fail, and returns its message.
    async fn err(&self, tool: &str, args: Value) -> String {
        let result = self.call(tool, args).await;
        let text = text_of(&result);
        assert_eq!(result.is_error, Some(true), "{tool} succeeded: {text}");
        text
    }

    /// Disconnects and waits for the server to close its browser.
    async fn stop(self) {
        self.client.cancel().await.expect("the client disconnects");
        self.server.await.expect("the server shuts down cleanly");
    }
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .unwrap_or_default()
}

fn page(body: &str) -> String {
    format!("data:text/html,<html lang=en><title>Shop</title>{body}</html>")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalog_lists_the_core_tools() {
    let h = Harness::start().await;
    let mut names: Vec<String> = h
        .client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "browser_status",
            "clear",
            "click",
            "evaluate",
            "focus",
            "go_back",
            "go_forward",
            "hover",
            "navigate",
            "press",
            "press_key",
            "reload",
            "screenshot",
            "scroll_into_view",
            "select_option",
            "set_checked",
            "snapshot",
            "tab_close",
            "tab_list",
            "tab_open",
            "tab_select",
            "type",
            "wait_for",
            "wait_for_element",
        ]
    );
    // Nothing has needed a browser yet.
    assert!(
        h.ok("browser_status", json!({}))
            .await
            .starts_with("No browser yet")
    );
    h.stop().await;
}

const FORM: &str = "<main><h1>Checkout</h1>\
    <label>Email <input id=email></label>\
    <label><input type=checkbox id=terms> Accept terms</label>\
    <select id=size><option value=s>Small<option value=l>Large</select>\
    <button onclick=\"document.querySelector('h1').textContent='Done: '+document.querySelector('%23email').value\">Pay</button>\
    <a href='https://shop.test/terms'>terms</a></main>";

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_a_page_and_acts_on_it_by_ref() {
    let h = Harness::start().await;
    let navigated = h
        .ok("navigate", json!({ "url": page(FORM), "snapshot": true }))
        .await;
    assert!(
        navigated.starts_with("Navigated to data:text/html"),
        "{navigated}"
    );
    assert!(navigated.contains(" | Shop\n\nSnapshot:\n"), "{navigated}");

    let tree = h.ok("snapshot", json!({})).await;
    assert_eq!(
        tree,
        r#"- RootWebArea "Shop" [ref=e1]
  - main
    - heading "Checkout" [ref=e2] [level=1]
    - LabelText
      - text: Email
      - textbox "Email" [ref=e3]
    - checkbox "Accept terms" [ref=e4]
    - combobox [ref=e5] [value="Small"]
      - MenuListPopup
        - option "Small" [ref=e6] [selected]
        - option "Large" [ref=e7]
    - button "Pay" [ref=e8]
    - link "terms" [ref=e9] [url=https://shop.test/terms]
"#
    );

    assert_eq!(
        h.ok("type", json!({ "ref": "e3", "text": "ada@example.com" }))
            .await,
        "Typed into e3"
    );
    h.ok("set_checked", json!({ "ref": "e4", "checked": true }))
        .await;
    h.ok("select_option", json!({ "ref": "e5", "values": ["l"] }))
        .await;
    let clicked = h
        .ok("click", json!({ "ref": "e8", "snapshot": true }))
        .await;
    assert!(
        clicked.starts_with("Clicked e8\n\nSnapshot:\n"),
        "{clicked}"
    );
    assert!(
        clicked.contains(r#"heading "Done: ada@example.com""#),
        "{clicked}"
    );
    assert!(clicked.contains("[checked]"), "{clicked}");
    assert!(clicked.contains(r#"[value="Large"]"#), "{clicked}");

    // A selector needs no snapshot.
    let evaluated = h
        .call(
            "evaluate",
            json!({ "expression": "el => el.value", "ref": "#email" }),
        )
        .await;
    assert_eq!(
        evaluated.structured_content,
        Some(json!({ "result": "ada@example.com" }))
    );
    let evaluated = h
        .call(
            "evaluate",
            json!({ "expression": "document.querySelectorAll('option').length" }),
        )
        .await;
    assert_eq!(evaluated.structured_content, Some(json!({ "result": 2 })));
    let evaluated = h
        .call("evaluate", json!({ "expression": "undefined" }))
        .await;
    assert_eq!(
        evaluated.structured_content,
        Some(json!({ "result": null }))
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mistakes_come_back_as_guidance() {
    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": page("<button>Go</button>") }))
        .await;
    let message = h.err("click", json!({ "ref": "e1" })).await;
    assert!(
        message.starts_with("No snapshot has been taken."),
        "{message}"
    );

    h.ok("snapshot", json!({})).await;
    let message = h.err("click", json!({ "ref": "e40" })).await;
    assert!(
        message.starts_with("Ref 'e40' is not in the latest snapshot."),
        "{message}"
    );

    // A navigation retires the refs it found.
    h.ok("navigate", json!({ "url": page("<button>Again</button>") }))
        .await;
    let message = h.err("click", json!({ "ref": "e2" })).await;
    assert!(
        message.starts_with("No snapshot has been taken."),
        "{message}"
    );

    let message = h
        .err("click", json!({ "ref": "button", "button": "side" }))
        .await;
    assert!(message.contains("Unknown button 'side'"), "{message}");
    let message = h
        .err(
            "click",
            json!({ "ref": "button", "double": true, "modifiers": ["Shift"] }),
        )
        .await;
    assert!(
        message.starts_with("A double-click uses the left button"),
        "{message}"
    );
    let message = h
        .err("navigate", json!({ "url": "file:///etc/passwd" }))
        .await;
    assert!(message.contains("file: URLs are blocked"), "{message}");
    let message = h
        .err(
            "evaluate",
            json!({ "expression": "document.title", "ref": "button" }),
        )
        .await;
    assert!(
        message.contains("does not evaluate to a function"),
        "{message}"
    );
    let message = h.err("wait_for", json!({})).await;
    assert_eq!(message, "Provide exactly one of time, text, or text_gone.");
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tabs_history_waits_and_screenshots() {
    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": page("<h1>One</h1>") }))
        .await;
    h.ok("navigate", json!({ "url": page("<h1>Two</h1>") }))
        .await;
    let back = h.ok("go_back", json!({})).await;
    assert!(
        back.starts_with("Navigated back to data:text/html"),
        "{back}"
    );
    assert!(
        h.ok("snapshot", json!({}))
            .await
            .contains(r#"heading "One""#)
    );
    h.ok("go_forward", json!({})).await;

    h.ok("tab_open", json!({ "url": page("<h1>Second tab</h1>") }))
        .await;
    let tabs = h.ok("tab_list", json!({})).await;
    assert_eq!(tabs.lines().count(), 2, "{tabs}");
    assert!(tabs.starts_with("[0] data:text/html"), "{tabs}");
    assert!(h.ok("snapshot", json!({})).await.contains("Second tab"));
    h.ok("tab_select", json!({ "index": 0 })).await;
    assert!(
        h.ok("snapshot", json!({}))
            .await
            .contains(r#"heading "Two""#)
    );
    let message = h.err("tab_select", json!({ "index": 5 })).await;
    assert!(
        message.starts_with("There is no tab 5: 2 tabs are open"),
        "{message}"
    );
    assert_eq!(
        h.ok("tab_close", json!({ "index": 1 })).await,
        "Closed tab 1."
    );
    assert_eq!(h.ok("tab_list", json!({})).await.lines().count(), 1);
    let status = h.ok("browser_status", json!({})).await;
    assert!(
        status.ends_with("1 context (active: default), 1 tab open."),
        "{status}"
    );

    h.ok(
        "navigate",
        json!({ "url": page("<script>setTimeout(() => document.body.append('Ready'), 300)</script>") }),
    )
    .await;
    assert_eq!(
        h.ok("wait_for", json!({ "text": "Ready" })).await,
        "Text appeared: Ready"
    );
    h.ok(
        "wait_for_element",
        json!({ "ref": "body", "state": "visible" }),
    )
    .await;

    let shot = h.call("screenshot", json!({})).await;
    let image = shot.content[0].as_image().expect("an image");
    assert_eq!(image.mime_type, "image/png");
    assert!(image.data.len() > 100);
    h.stop().await;
}

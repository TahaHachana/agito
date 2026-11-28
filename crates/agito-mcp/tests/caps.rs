//! The tool groups added with `--caps`: coordinates, contexts and routing. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

mod common;

use agito_mcp::{Capability, ServerOptions};
use common::{Harness, http_server, page};
use serde_json::{Value, json};

fn with(capabilities: &[Capability]) -> ServerOptions {
    ServerOptions {
        capabilities: capabilities.to_vec(),
        ..ServerOptions::default()
    }
}

async fn names(h: &Harness) -> Vec<String> {
    h.client
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect()
}

async fn eval(h: &Harness, expression: &str) -> Value {
    h.call("evaluate", json!({ "expression": expression }))
        .await
        .structured_content
        .expect("a value")["result"]
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn groups_only_add_to_the_catalog() {
    let plain = Harness::start().await;
    let base = names(&plain).await;
    assert_eq!(base.len(), 31);
    assert!(!base.contains(&"click_xy".to_owned()));
    plain.stop().await;

    let all = Harness::with(with(&[
        Capability::Coordinates,
        Capability::Contexts,
        Capability::Routing,
        Capability::Routing,
    ]))
    .await;
    let every = names(&all).await;
    assert!(base.iter().all(|n| every.contains(n)), "nothing is dropped");
    let mut added: Vec<String> = every.into_iter().filter(|n| !base.contains(n)).collect();
    added.sort();
    assert_eq!(
        added,
        [
            "click_xy",
            "context_close",
            "context_create",
            "context_list",
            "context_select",
            "drag",
            "hover_xy",
            "move_xy",
            "resize",
            "route_abort",
            "route_continue",
            "route_fulfill",
            "route_list",
            "scroll_xy",
            "unroute",
        ]
    );
    all.stop().await;
}

/// A canvas that records where it is clicked and hovered, a box dragged with pointer events,
/// and a tall body to scroll.
const SURFACE: &str = "<body style='margin:0;height:3000px'>\
    <canvas id=c width=200 height=100 style='display:block'></canvas>\
    <div id=box style='position:absolute;left:300px;top:20px;width:40px;height:40px;background:red'></div>\
    <div id=target style='position:absolute;left:500px;top:20px;width:60px;height:60px;background:blue'></div>\
    <script>\
    const c = document.getElementById('c');\
    c.addEventListener('click', e => document.title = 'click ' + e.offsetX + ',' + e.offsetY + (e.shiftKey ? ' shift' : ''));\
    c.addEventListener('mousemove', e => window.hovered = [e.offsetX, e.offsetY]);\
    const box = document.getElementById('box'); let drag = null; window.moves = 0;\
    box.addEventListener('mousedown', e => drag = [e.clientX - box.offsetLeft, e.clientY - box.offsetTop]);\
    document.addEventListener('mousemove', e => { if (drag) { window.moves++; box.style.left = (e.clientX - drag[0]) + 'px'; box.style.top = (e.clientY - drag[1]) + 'px'; } });\
    document.addEventListener('mouseup', () => drag = null);\
    </script></body>";

#[tokio::test(flavor = "multi_thread")]
async fn coordinates_reach_what_has_no_ref() {
    let h = Harness::with(with(&[Capability::Coordinates])).await;
    h.ok("navigate", json!({ "url": page(SURFACE) })).await;

    let clicked = h
        .ok(
            "click_xy",
            json!({ "x": 40, "y": 30, "modifiers": ["Shift"] }),
        )
        .await;
    assert!(
        clicked.starts_with("Clicked at (40, 30)\nPage: "),
        "{clicked}"
    );
    assert_eq!(eval(&h, "document.title").await, "click 40,30 shift");

    assert_eq!(
        h.ok("hover_xy", json!({ "x": 15, "y": 25 })).await,
        "Hovering at (15, 25)"
    );
    assert_eq!(eval(&h, "window.hovered").await, json!([15, 25]));

    // By coordinates: grab the box 10px into it and drop it at (410, 210).
    h.ok(
        "drag",
        json!({ "start_x": 310, "start_y": 30, "end_x": 410, "end_y": 210, "steps": 5 }),
    )
    .await;
    assert_eq!(
        eval(&h, "[box.offsetLeft, box.offsetTop, window.moves]").await,
        json!([400, 200, 5])
    );
    // By elements: the box's centre onto the target's.
    let dragged = h
        .ok("drag", json!({ "start_ref": "#box", "end_ref": "#target" }))
        .await;
    assert_eq!(dragged, "Dragged #box to #target");
    assert_eq!(
        eval(&h, "[box.offsetLeft, box.offsetTop]").await,
        json!([510, 30])
    );
    let mixed = h
        .err(
            "drag",
            json!({ "start_ref": "#box", "end_x": 1, "end_y": 1 }),
        )
        .await;
    assert!(
        mixed.starts_with("Provide either start_ref and end_ref"),
        "{mixed}"
    );

    h.ok(
        "scroll_xy",
        json!({ "x": 100, "y": 300, "delta_x": 0, "delta_y": 600 }),
    )
    .await;
    h.ok("wait_for", json!({ "time": 300 })).await;
    assert_eq!(eval(&h, "window.scrollY").await, 600);

    assert_eq!(
        h.ok("resize", json!({ "width": 640, "height": 480 })).await,
        "Viewport resized to 640x480"
    );
    assert_eq!(
        eval(&h, "[innerWidth, innerHeight]").await,
        json!([640, 480])
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_page_with_nothing_to_address_points_at_the_tools_this_server_has() {
    // No title: a named document would itself be addressable.
    let canvas = "data:text/html,<canvas width=50 height=50></canvas>";
    for (options, next) in [
        (
            with(&[Capability::Coordinates]),
            "Take a screenshot to identify controls visually, then act on their positions with \
             the coordinate tools: click_xy, drag, or scroll_xy.\n",
        ),
        (
            ServerOptions::default(),
            "Take a screenshot to identify controls visually. This server has no tools that act \
             on a position, so nothing on this page can be clicked until it is restarted with \
             --caps coordinates.\n",
        ),
    ] {
        let h = Harness::with(options).await;
        h.ok("navigate", json!({ "url": canvas })).await;
        let tree = h.ok("snapshot", json!({})).await;
        assert!(
            tree.contains("Note: no addressable elements were found."),
            "{tree}"
        );
        assert!(tree.ends_with(next), "{tree}");
        h.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn contexts_keep_their_cookies_and_storage_apart() {
    let origin = http_server(|_, _| (200, "text/html", "<title>Account</title>".into())).await;
    let h = Harness::with(with(&[Capability::Contexts])).await;
    assert_eq!(
        h.ok("context_list", json!({})).await,
        "No contexts are open yet; 'default' becomes active on first use."
    );
    h.ok("navigate", json!({ "url": format!("{origin}/") }))
        .await;
    h.ok(
        "evaluate",
        json!({ "expression": "document.cookie = 'user=author'; localStorage.draft = 'mine'" }),
    )
    .await;

    assert_eq!(
        h.ok("context_create", json!({ "name": "reviewer" })).await,
        "Created context 'reviewer'."
    );
    h.ok("navigate", json!({ "url": format!("{origin}/") }))
        .await;
    assert_eq!(eval(&h, "document.cookie").await, "");
    assert_eq!(eval(&h, "localStorage.draft ?? null").await, json!(null));
    assert_eq!(
        h.ok("context_list", json!({})).await,
        "  default\n* reviewer"
    );

    let tabs = h.ok("tab_list", json!({})).await;
    assert_eq!(
        tabs,
        format!(
            "[0] {origin}/ | Account | context: default\n[1] {origin}/ | Account | context: reviewer"
        )
    );
    // Selecting a tab in another context switches to it.
    h.ok("tab_select", json!({ "index": 0 })).await;
    assert_eq!(eval(&h, "document.cookie").await, "user=author");
    assert!(
        h.ok("context_list", json!({}))
            .await
            .starts_with("* default")
    );

    let taken = h.err("context_create", json!({ "name": "reviewer" })).await;
    assert!(
        taken.starts_with("A context named 'reviewer' is already open"),
        "{taken}"
    );
    let missing = h.err("context_select", json!({ "name": "editor" })).await;
    assert!(
        missing.starts_with("No open context is named 'editor'"),
        "{missing}"
    );

    h.ok("context_select", json!({ "name": "reviewer" })).await;
    h.ok("context_close", json!({ "name": "reviewer" })).await;
    assert_eq!(h.ok("context_list", json!({})).await, "* default");
    assert_eq!(h.ok("tab_list", json!({})).await.lines().count(), 1);
    h.stop().await;
}

fn api(_: &str, path: &str) -> (u16, &'static str, String) {
    match path {
        "/" => (
            200,
            "text/html",
            "<title>Status</title><script>\
             fetch('/api/status.json').then(r => r.text()).then(t => document.title = t, () => document.title = 'failed')\
             </script>"
                .into(),
        ),
        "/api/status.json" => (200, "application/json", r#"{"overall":"healthy"}"#.into()),
        "/api/v2/status.json" => (200, "application/json", r#"{"overall":"v2"}"#.into()),
        _ => (404, "text/plain", "missing".into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_mock_block_and_redirect_requests() {
    let origin = http_server(api).await;
    let h = Harness::with(with(&[Capability::Routing])).await;
    let status = || async {
        h.ok("navigate", json!({ "url": format!("{origin}/") }))
            .await;
        h.ok("wait_for", json!({ "time": 300 })).await;
        eval(&h, "document.title").await
    };
    assert_eq!(status().await, r#"{"overall":"healthy"}"#);

    assert_eq!(
        h.ok(
            "route_fulfill",
            json!({
                "url_pattern": "*/api/status.json",
                "content_type": "application/json",
                "body": r#"{"overall":"degraded"}"#
            }),
        )
        .await,
        "Mocking '*/api/status.json' with status 200."
    );
    assert_eq!(status().await, r#"{"overall":"degraded"}"#);

    // A later pattern that also matches wins.
    h.ok("route_abort", json!({ "url_pattern": "/api/" })).await;
    assert_eq!(status().await, "failed");
    assert_eq!(
        h.ok("route_list", json!({})).await,
        "*/api/status.json -> Fulfill\n/api/ -> Abort"
    );
    // Re-registering keeps the pattern's place: /api/ is still the newer one.
    h.ok(
        "route_fulfill",
        json!({ "url_pattern": "*/api/status.json", "body": "again" }),
    )
    .await;
    assert_eq!(status().await, "failed");

    assert_eq!(
        h.ok("unroute", json!({ "url_pattern": "/api/" })).await,
        "Removed the mock for '/api/'."
    );
    assert_eq!(
        h.ok("unroute", json!({ "url_pattern": "/api/" })).await,
        "No mock was registered for '/api/'."
    );
    assert_eq!(status().await, "again");
    h.ok("unroute", json!({ "url_pattern": "*/api/status.json" }))
        .await;
    assert_eq!(status().await, r#"{"overall":"healthy"}"#);

    h.ok(
        "route_continue",
        json!({ "url_pattern": "/api/status.json", "url": format!("{origin}/api/v2/status.json") }),
    )
    .await;
    assert_eq!(status().await, r#"{"overall":"v2"}"#);

    let refused = h
        .err(
            "route_continue",
            json!({ "url_pattern": "/x", "url": "file:///etc/passwd" }),
        )
        .await;
    assert!(refused.contains("file: URLs are blocked"), "{refused}");
    let refused = h
        .err(
            "route_fulfill",
            json!({ "url_pattern": "/x", "status": 302, "headers": { "Location": "file:///etc" } }),
        )
        .await;
    assert!(refused.contains("file: URLs are blocked"), "{refused}");
    h.stop().await;
}

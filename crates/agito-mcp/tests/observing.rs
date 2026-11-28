//! What an agent learns about what happened: action reports, dialogs, the console and network
//! logs, frames, and the accessibility audit. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use agito_mcp::{DialogPolicy, ServerOptions};
use common::{Harness, http_server, page};
use serde_json::json;

/// Pages for following links: a page can not open a `data:` URL in a top-level window.
fn links(_: &str, path: &str) -> (u16, &'static str, String) {
    let body = match path {
        "/away" => "<title>Away</title>",
        "/popup" => "<title>Popup</title>",
        _ => {
            "<title>Shop</title>\
             <button id=log onclick=\"console.error('boom'); setTimeout(() => { throw new Error('later') })\">Log</button>\
             <button id=title onclick=\"document.title='Renamed'\">Rename</button>\
             <a id=away href=/away>Away</a>\
             <a id=popup target=_blank href=/popup>Popup</a>"
        }
    };
    (200, "text/html", body.into())
}

#[tokio::test(flavor = "multi_thread")]
async fn actions_report_what_they_changed() {
    let origin = http_server(links).await;
    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": format!("{origin}/") }))
        .await;

    // Nothing surprising: one line.
    assert_eq!(
        h.ok("focus", json!({ "ref": "#log" })).await,
        "Focused #log"
    );

    let logged = h.ok("click", json!({ "ref": "#log" })).await;
    assert_eq!(
        logged,
        "Clicked #log\nConsole: 1 error, 1 page error. Read them with console_messages since=1."
    );
    let console = h.ok("console_messages", json!({ "since": 1 })).await;
    assert!(
        console.starts_with("[error] boom\n[pageerror] Error: later\n    at "),
        "{console}"
    );
    assert!(console.ends_with("\nnext=3"), "{console}");
    assert_eq!(
        h.ok("console_messages", json!({ "since": 3 })).await,
        "No console messages have been logged since 3.\nnext=3"
    );

    let renamed = h.ok("click", json!({ "ref": "#title" })).await;
    assert_eq!(
        renamed,
        format!("Clicked #title\nPage: {origin}/ | Renamed")
    );

    let opened = h.ok("click", json!({ "ref": "#popup" })).await;
    assert_eq!(
        opened,
        format!("Clicked #popup\nNew tab opened: [1] {origin}/popup")
    );

    h.ok("snapshot", json!({})).await;
    let away = h.ok("click", json!({ "ref": "#away" })).await;
    assert!(
        away.starts_with(&format!("Clicked #away\nPage: {origin}/away | Away\n")),
        "{away}"
    );
    assert!(
        away.ends_with(
            "Refs from the last snapshot no longer address this page: it navigated. Take a new \
             snapshot."
        ),
        "{away}"
    );
    let message = h.err("click", json!({ "ref": "e2" })).await;
    assert!(
        message.starts_with("The page navigated since the last snapshot"),
        "{message}"
    );
    h.stop().await;
}

const DIALOGS: &str = "<button id=alert onclick=\"alert('Saved')\">Alert</button>\
    <button id=prompt onclick=\"document.title = prompt('Name?') || 'none'\">Prompt</button>";

#[tokio::test(flavor = "multi_thread")]
async fn a_dialog_interrupts_the_action_and_blocks_the_page_until_handled() {
    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": page(DIALOGS) })).await;

    let clicked = h.ok("click", json!({ "ref": "#alert" })).await;
    assert!(
        clicked.starts_with("The action opened an alert dialog: \"Saved\". Call handle_dialog"),
        "{clicked}"
    );
    // Anything that needs the page is refused, and every result says why.
    let refused = h.err("snapshot", json!({})).await;
    assert!(
        refused.contains("A \"alert\" dialog is open: \"Saved\""),
        "{refused}"
    );
    let listed = h.call("tab_list", json!({})).await;
    assert!(
        common::text_of(&listed).starts_with("A \"alert\" dialog is open"),
        "{listed:?}"
    );
    assert_eq!(
        h.ok("handle_dialog", json!({ "accept": true })).await,
        "Dialog (alert) accepted: Saved"
    );
    assert_eq!(
        h.err("handle_dialog", json!({ "accept": true })).await,
        "No dialog is open."
    );
    h.ok("snapshot", json!({})).await;

    let clicked = h.ok("click", json!({ "ref": "#prompt" })).await;
    assert!(
        clicked.starts_with("The action opened a prompt dialog"),
        "{clicked}"
    );
    h.ok("handle_dialog", json!({ "accept": true, "text": "Ada" }))
        .await;
    let title = h
        .call("evaluate", json!({ "expression": "document.title" }))
        .await;
    assert_eq!(title.structured_content, Some(json!({ "result": "Ada" })));
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dialog_policy_answers_dialogs_itself() {
    let h = Harness::with(ServerOptions {
        dialogs: DialogPolicy::Accept,
        ..ServerOptions::default()
    })
    .await;
    h.ok("navigate", json!({ "url": page(DIALOGS) })).await;
    assert_eq!(
        h.ok("click", json!({ "ref": "#alert" })).await,
        "Clicked #alert"
    );
    h.ok("snapshot", json!({})).await;
    h.stop().await;
}

fn shop(method: &str, path: &str) -> (u16, &'static str, String) {
    match (method, path) {
        ("GET", "/") => (
            200,
            "text/html",
            "<title>Shop</title><script>fetch('/api/cart', { method: 'POST', body: '{\"id\":7}' })\
             .then(r => r.text()).then(t => document.title = t)</script>"
                .into(),
        ),
        ("POST", "/api/cart") => (201, "application/json", r#"{"items":1}"#.into()),
        _ => (404, "text/plain", "missing".into()),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_network_log_shows_requests_and_their_detail() {
    let origin = http_server(shop).await;
    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": format!("{origin}/") }))
        .await;
    h.ok("wait_for", json!({ "time": 300 })).await;

    let log = h.ok("network_requests", json!({})).await;
    let lines: Vec<&str> = log.lines().collect();
    assert!(
        lines.contains(&format!("[1] GET 200 {origin}/ (Document)").as_str()),
        "{log}"
    );
    let cart = lines
        .iter()
        .find(|l| l.contains("/api/cart"))
        .unwrap_or_else(|| panic!("{log}"));
    assert!(
        cart.ends_with(&format!("POST 201 {origin}/api/cart (Fetch)")),
        "{log}"
    );
    let sequence: u64 = cart[1..cart.find(']').unwrap()].parse().unwrap();

    let detail = h
        .ok("network_request", json!({ "sequence": sequence }))
        .await;
    assert!(detail.contains("Request body:\n{\"id\":7}\n"), "{detail}");
    assert!(
        detail.contains("  content-type: application/json\n"),
        "{detail}"
    );
    assert!(
        detail.ends_with("Response body: \n{\"items\":1}"),
        "{detail}"
    );
    let missing = h.err("network_request", json!({ "sequence": 99 })).await;
    assert!(
        missing.starts_with("No request with sequence 99"),
        "{missing}"
    );
    h.stop().await;
}

const FRAMED: &str = "<main><h1>Checkout</h1>\
    <iframe title=Payment srcdoc=\"<label>Card <input></label><button onclick=&quot;parent.document.title='Paid'&quot;>Pay</button>\"></iframe>\
    <iframe style=display:none srcdoc=\"<p>hidden</p>\"></iframe></main>";

#[tokio::test(flavor = "multi_thread")]
async fn frames_print_inside_the_page_and_their_refs_reach_into_them() {
    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": page(FRAMED) })).await;
    h.ok("wait_for", json!({ "time": 200 })).await;

    let tree = h.ok("snapshot", json!({})).await;
    assert!(
        tree.contains(
            "    - Iframe \"Payment\" [ref=e3] [frame=1]\n\
             \x20     - generic\n\
             \x20       - LabelText\n\
             \x20         - text: Card\n\
             \x20         - textbox \"Card\" [ref=f1e1]\n\
             \x20       - button \"Pay\" [ref=f1e2]\n"
        ),
        "{tree}"
    );
    assert!(
        tree.ends_with(
            "Note: this page has 1 more frame whose contents are not in this tree. Use \
             frame_list to see them and frame_select to look inside one.\n"
        ),
        "{tree}"
    );
    h.ok("type", json!({ "ref": "f1e1", "text": "4242" })).await;
    let paid = h.ok("click", json!({ "ref": "f1e2" })).await;
    assert!(paid.ends_with(" | Paid"), "{paid}");

    let frames = h.ok("frame_list", json!({})).await;
    assert!(
        frames.starts_with("* [0] (page) data:text/html"),
        "{frames}"
    );
    assert!(
        frames.contains("\n  [1]   about:srcdoc\n  [2]   about:srcdoc"),
        "{frames}"
    );

    h.ok("frame_select", json!({ "index": 1 })).await;
    let scoped = h.ok("snapshot", json!({})).await;
    assert!(
        scoped.starts_with("Scoped to frame about:srcdoc\n\n"),
        "{scoped}"
    );
    assert!(
        scoped.contains("      - textbox \"Card\" [ref=e2] [value=\"4242\"]\n"),
        "{scoped}"
    );
    let value = h
        .call(
            "evaluate",
            json!({ "expression": "document.querySelector('input').value" }),
        )
        .await;
    assert_eq!(value.structured_content, Some(json!({ "result": "4242" })));
    assert!(h.ok("frame_list", json!({})).await.contains("* [1]"));
    let back = h.ok("frame_select", json!({ "index": 0 })).await;
    assert!(back.starts_with("Scoped back to the page"), "{back}");
    let missing = h.err("frame_select", json!({ "index": 9 })).await;
    assert!(
        missing.starts_with("There is no frame 9: the page has 3 frames"),
        "{missing}"
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_audit_points_at_elements_by_ref_where_it_can() {
    let h = Harness::start().await;
    h.ok(
        "navigate",
        json!({ "url": "data:text/html,<html lang=en><main><button id=icon><svg width=9 height=9></svg></button><img src='data:,'></main></html>" }),
    )
    .await;
    let audit = h.call("audit_accessibility", json!({})).await;
    let summary = common::text_of(&audit);
    assert!(
        summary.starts_with("Found 2 accessibility violation(s):"),
        "{summary}"
    );
    let violations = audit.structured_content.unwrap()["violations"].clone();
    let by_rule = |rule: &str| {
        violations
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["ruleId"] == rule)
            .cloned()
            .unwrap_or_else(|| panic!("{violations}"))
    };
    let button = by_rule("a11y-empty-button");
    assert_eq!(button["severity"], "error");
    assert_eq!(button["selector"], "#icon");
    let r = button["ref"].as_str().expect("the button took a ref");
    // The ref is one the audit's own snapshot handed out.
    h.ok("focus", json!({ "ref": r })).await;
    let image = by_rule("a11y-alt-text");
    assert_eq!(image["ref"], json!(null), "an unnamed image takes no ref");
    assert_eq!(image["selector"], "html > body > main > img");

    let none = h
        .ok("audit_accessibility", json!({ "min_severity": "warning" }))
        .await;
    assert!(none.starts_with("Found 2"), "{none}");
    let bad = h
        .err("audit_accessibility", json!({ "min_severity": "fatal" }))
        .await;
    assert_eq!(
        bad,
        "Unknown severity 'fatal'. Use error, warning, or info."
    );
    h.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_navigating_on_its_own_retires_its_refs() {
    let h = Harness::start().await;
    h.ok(
        "navigate",
        json!({ "url": page(
            "<iframe title=Widget srcdoc=\"<button onclick=&quot;location.href='about:blank'&quot;>Leave</button>\"></iframe>"
        ) }),
    )
    .await;
    h.ok("wait_for", json!({ "time": 200 })).await;
    let tree = h.ok("snapshot", json!({})).await;
    assert!(tree.contains("button \"Leave\" [ref=f1e1]"), "{tree}");
    let left = h.ok("click", json!({ "ref": "f1e1" })).await;
    assert_eq!(
        left,
        "Clicked f1e1\nRefs inside frame 1 no longer address anything: the frame navigated. \
         Take a new snapshot."
    );
    h.stop().await;
}

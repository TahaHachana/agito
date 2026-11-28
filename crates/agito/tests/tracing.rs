//! Traces: what a context did, saved as a zip. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agito::{BrowserContext, Page, TracingOptions, expect};
use common::serve;
use serde_json::Value;

const SHOP: &str = "<!doctype html><title>Shop</title>\
    <input id=q><button id=b onclick=\"console.error('cart is empty'); alert('Added')\">Buy</button>\
    <script>console.log('ready'); fetch('/api/cart').then(r => r.text())</script>";

fn scratch(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("agito-trace-{name}-{}.zip", std::process::id()))
}

/// The zip's files, by name.
fn unzip(path: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..archive.len())
        .map(|i| {
            let mut file = archive.by_index(i).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            (file.name().unwrap().into_owned(), bytes)
        })
        .collect()
}

#[agito::test]
async fn a_trace_records_what_the_context_did(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    let base = serve(&[("/", 200, SHOP), ("/api/cart", 200, "[]")]).await;
    context
        .start_tracing(TracingOptions {
            title: Some("checkout".into()),
            ..Default::default()
        })
        .await?;
    assert!(context.is_tracing());
    page.goto(&base).await?;
    page.locator("#q").fill("shoes").await?;
    page.locator("#b").click().await?;
    // The dialog is dismissed as usual: tracing only watches.
    expect(&page.locator("#q")).to_have_value("shoes").await?;
    let path = scratch("records");
    context.stop_tracing(&path).await?;
    assert!(!context.is_tracing());

    let files = unzip(&path);
    let log: Value = serde_json::from_slice(&files["trace.json"]).unwrap();
    assert_eq!(log["title"], "checkout");
    assert_eq!(log["pages"], serde_json::json!(["page-1"]));
    let events = log["events"].as_array().unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e["type"].as_str().unwrap()).collect();
    for kind in ["trace", "navigation", "console", "action", "dialog"] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }
    let navigation = events.iter().find(|e| e["type"] == "navigation").unwrap();
    assert_eq!(navigation["status"], 200);
    assert_eq!(navigation["url"], format!("{base}/"));
    let actions: Vec<(&str, &str)> = events
        .iter()
        .filter(|e| e["type"] == "action")
        .map(|e| (e["action"].as_str().unwrap(), e["target"].as_str().unwrap()))
        .collect();
    assert_eq!(actions, [("fill", "#q"), ("click", "#b")]);
    let click = events.iter().find(|e| e["action"] == "click").unwrap();
    assert!(
        click["duration"].as_f64().is_some_and(|d| d > 0.0),
        "{click}"
    );
    assert_eq!(click["error"], Value::Null);
    assert!(
        events.iter().any(|e| e["type"] == "console"
            && e["level"] == "error"
            && e["text"] == "cart is empty"),
        "{events:?}"
    );
    let dialog = events.iter().find(|e| e["type"] == "dialog").unwrap();
    assert_eq!(
        (dialog["kind"].as_str(), dialog["message"].as_str()),
        (Some("alert"), Some("Added"))
    );
    // Times only go forward.
    let times: Vec<f64> = events.iter().map(|e| e["time"].as_f64().unwrap()).collect();
    assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");

    // Each screenshot an event names is in the zip, as a JPEG.
    let shots: Vec<&str> = events
        .iter()
        .filter_map(|e| e["screenshot"].as_str())
        .collect();
    assert!(shots.len() >= 2, "{shots:?}");
    for shot in shots {
        assert!(files[shot].starts_with(&[0xFF, 0xD8]), "{shot} is a JPEG");
    }

    let har: Value = serde_json::from_slice(&files["har.json"]).unwrap();
    let urls: Vec<&str> = har["log"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["request"]["url"].as_str().unwrap())
        .collect();
    assert!(urls.iter().any(|u| u.ends_with("/api/cart")), "{urls:?}");
    assert!(
        !files.contains_key("chrome-trace.json"),
        "the timeline is opt-in"
    );
    std::fs::remove_file(path).unwrap();
    Ok(())
}

#[agito::test]
async fn tracing_is_started_and_stopped_once(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    let err = context.stop_tracing(scratch("never")).await.unwrap_err();
    assert!(err.to_string().contains("no trace is running"), "{err}");
    context
        .start_tracing(TracingOptions {
            screenshots: false,
            ..Default::default()
        })
        .await?;
    let err = context.start_tracing(Default::default()).await.unwrap_err();
    assert!(err.to_string().contains("already running"), "{err}");
    page.set_content("<button>x</button>").await?;
    page.locator("button").click().await?;
    let path = scratch("no-shots");
    context.stop_tracing(&path).await?;
    let files = unzip(&path);
    assert!(
        !files.keys().any(|k| k.starts_with("resources/")),
        "{:?}",
        files.keys()
    );
    std::fs::remove_file(path).unwrap();
    // A discarded trace writes nothing.
    context.start_tracing(Default::default()).await?;
    context.discard_tracing().await?;
    assert!(!context.is_tracing());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn contexts_trace_at_once_and_timelines_take_turns() -> agito::Result<()> {
    let browser = agito::launch(Default::default()).await?;
    let first = browser.new_context(Default::default()).await?;
    let second = browser.new_context(Default::default()).await?;
    // Event logs: both at once, with nothing to wait for.
    first.start_tracing(Default::default()).await?;
    second.start_tracing(Default::default()).await?;
    second.discard_tracing().await?;
    first.discard_tracing().await?;

    // Timelines: Chrome records one at a time, so the second waits for the first.
    let timeline = TracingOptions {
        timeline: true,
        ..Default::default()
    };
    first.start_tracing(timeline.clone()).await?;
    let page = first.new_page().await?;
    page.set_content("<h1>timed</h1>").await?;
    let waiting = {
        let second = second.clone();
        let timeline = timeline.clone();
        tokio::spawn(async move { second.start_tracing(timeline).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!waiting.is_finished(), "the second timeline waits");
    let path = scratch("timeline");
    first.stop_tracing(&path).await?;
    waiting.await.unwrap()?;
    second.discard_tracing().await?;

    let files = unzip(&path);
    let chrome: Value = serde_json::from_slice(&files["chrome-trace.json"]).unwrap();
    let events = chrome["traceEvents"]
        .as_array()
        .or(chrome.as_array())
        .expect("trace events");
    assert!(events.len() > 10, "{} events", events.len());
    std::fs::remove_file(path).unwrap();
    browser.close().await
}

#[agito::test]
async fn a_trace_leaves_the_page_s_own_har_recording_alone(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    let base = serve(&[("/", 200, "<title>Shop</title>")]).await;
    page.start_har();
    context.start_tracing(Default::default()).await?;
    page.goto(&base).await?;
    let path = scratch("own-har");
    context.stop_tracing(&path).await?;
    // The page's recording is still running, with what the trace also saw.
    let har = page
        .stop_har()
        .expect("the page's HAR recording survived the trace");
    let urls = |log: &Value| -> Vec<String> {
        log["log"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["request"]["url"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(urls(&har).contains(&format!("{base}/")), "{har}");
    let files = unzip(&path);
    let traced: Value = serde_json::from_slice(&files["har.json"]).unwrap();
    assert!(urls(&traced).contains(&format!("{base}/")), "{traced}");
    std::fs::remove_file(path).unwrap();
    Ok(())
}

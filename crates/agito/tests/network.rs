//! Network events, routing and HAR. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agito::{BrowserContext, ContinueOptions, FulfillOptions, Page, expect};
use common::serve;
use serde_json::{Value, json};

const APP: &str = r#"<button id="load" onclick="
    fetch('/api/data').then(r => r.json()).then(d => document.getElementById('out').textContent = d.name)
      .catch(e => document.getElementById('out').textContent = 'error: ' + e.message)">load</button>
  <button id="post" onclick="
    fetch('/echo', { method: 'POST', body: JSON.stringify({ q: 'rust' }) }).then(r => r.json())
      .then(d => document.getElementById('out').textContent = d.body)">post</button>
  <button id="slow" onclick="fetch('/slow').then(() => window.slowDone = true)">slow</button>
  <p id="out"></p>"#;

async fn app() -> String {
    serve(&[
        ("/", 200, APP),
        ("/api/data", 200, r#"{"name":"server"}"#),
        ("/slow", 200, "done"),
    ])
    .await
}

#[agito::test]
async fn request_and_response_events(page: Page) -> agito::Result<()> {
    let base = app().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    page.on_request(move |r| {
        sink.lock()
            .unwrap()
            .push(format!("{} {}", r.method(), r.url()))
    });
    let finished = Arc::new(AtomicUsize::new(0));
    let counter = finished.clone();
    page.on_request_finished(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    page.goto(&base).await?;

    let response = page.wait_for_response("**/api/data", None);
    page.locator("#load").click().await?;
    let response = response.await?;
    assert_eq!(response.status(), 200);
    assert!(response.ok());
    assert_eq!(response.header("content-type"), Some("application/json"));
    let body: Value = response.json().await?;
    assert_eq!(body, json!({ "name": "server" }));
    let request = response
        .request()
        .expect("a browser response has its request");
    assert_eq!(request.method(), "GET");
    assert_eq!(request.resource_type(), "Fetch");
    assert!(request.frame().unwrap().is_main());
    expect(&page.locator("#out")).to_have_text("server").await?;

    let seen = seen.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.ends_with("/api/data")), "{seen:?}");
    assert!(
        seen.iter().any(|s| s == &format!("GET {base}/")),
        "{seen:?}"
    );
    assert!(finished.load(Ordering::SeqCst) >= 2);
    Ok(())
}

#[agito::test]
async fn post_bodies_are_visible(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    let request = page.wait_for_request("**/echo", None);
    page.locator("#post").click().await?;
    let request = request.await?;
    assert_eq!(request.method(), "POST");
    assert_eq!(
        request.post_data_json::<Value>()?,
        Some(json!({ "q": "rust" }))
    );
    expect(&page.locator("#out"))
        .to_have_text(r#"{"q":"rust"}"#)
        .await?;
    Ok(())
}

#[agito::test]
async fn routes_fulfill_with_mock_data(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    page.route("**/api/data", move |route| {
        counter.fetch_add(1, Ordering::SeqCst);
        async move {
            let _ = route
                .fulfill(FulfillOptions::json(&json!({ "name": "mocked" })))
                .await;
        }
    })
    .await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out")).to_have_text("mocked").await?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[agito::test]
async fn routes_abort_and_report_failures(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    let failures = Arc::new(Mutex::new(Vec::new()));
    let sink = failures.clone();
    page.on_request_failed(move |r| sink.lock().unwrap().push(r.failure().unwrap_or_default()));
    page.route("/api/data", |route| async move {
        let _ = route.abort(Some("connectionrefused")).await;
    })
    .await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out"))
        .to_contain_text("error:")
        .await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while failures.lock().unwrap().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        failures
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.contains("ERR_CONNECTION_REFUSED")),
        "{failures:?}"
    );
    Ok(())
}

#[agito::test]
async fn routes_continue_with_changes(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    page.route("**/echo", |route| async move {
        let _ = route
            .continue_(ContinueOptions {
                post_data: Some("rewritten".into()),
                ..Default::default()
            })
            .await;
    })
    .await?;
    page.locator("#post").click().await?;
    expect(&page.locator("#out"))
        .to_have_text("rewritten")
        .await?;
    Ok(())
}

#[agito::test]
async fn unhandled_and_panicking_routes_let_requests_through(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    let id = page.route("**/api/data", |_route| async move {}).await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out")).to_have_text("server").await?;

    page.unroute(id).await?;
    page.route("**/api/data", |_route| async move { panic!("handler bug") })
        .await?;
    page.evaluate::<()>("document.getElementById('out').textContent = ''")
        .await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out")).to_have_text("server").await?;

    page.unroute_all().await?;
    page.evaluate::<()>("document.getElementById('out').textContent = ''")
        .await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out")).to_have_text("server").await?;
    Ok(())
}

#[agito::test]
async fn documents_can_be_served_from_routes(page: Page) -> agito::Result<()> {
    page.route("http://agito.test/**", |route| async move {
        let _ = route
            .fulfill(FulfillOptions::html("<h1>fake site</h1>").with_status(203))
            .await;
    })
    .await?;
    let response = page
        .goto("http://agito.test/home")
        .await?
        .expect("document response");
    assert_eq!(response.status(), 203);
    expect(&page.locator("h1"))
        .to_have_text("fake site")
        .await?;
    Ok(())
}

#[agito::test]
async fn context_routes_apply_to_new_pages_and_page_routes_win(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    let base = app().await;
    context
        .route("**/api/data", |route| async move {
            let _ = route
                .fulfill(FulfillOptions::json(&json!({ "name": "context" })))
                .await;
        })
        .await?;
    let second = context.new_page().await?;
    second.goto(&base).await?;
    second.locator("#load").click().await?;
    expect(&second.locator("#out"))
        .to_have_text("context")
        .await?;

    page.goto(&base).await?;
    page.route("**/api/data", |route| async move {
        let _ = route
            .fulfill(FulfillOptions::json(&json!({ "name": "page" })))
            .await;
    })
    .await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out")).to_have_text("page").await?;
    Ok(())
}

#[agito::test]
async fn redirects_are_followed_and_reported(page: Page) -> agito::Result<()> {
    let base = serve(&[("/old", 302, "REDIRECT:/new"), ("/new", 200, "<p>new</p>")]).await;
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let sink = statuses.clone();
    page.on_response(move |r| sink.lock().unwrap().push((r.status(), r.url().to_owned())));
    let response = page.goto(&format!("{base}/old")).await?.unwrap();
    assert!(response.url().ends_with("/new"), "{}", response.url());
    assert_eq!(response.status(), 200);
    let statuses = statuses.lock().unwrap().clone();
    assert!(
        statuses
            .iter()
            .any(|(s, u)| *s == 302 && u.ends_with("/old")),
        "{statuses:?}"
    );
    Ok(())
}

#[agito::test]
async fn waits_for_the_network_to_go_quiet(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    page.locator("#slow").click().await?;
    page.wait_for_network_idle(Duration::from_millis(300), None)
        .await?;
    let done: bool = page.evaluate("window.slowDone === true").await?;
    assert!(done);
    Ok(())
}

#[agito::test]
async fn waiting_for_a_response_that_never_comes_times_out(page: Page) -> agito::Result<()> {
    page.goto(&app().await).await?;
    let err = page
        .wait_for_response("**/never", Some(Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert!(err.is_timeout(), "{err:?}");
    Ok(())
}

#[agito::test]
async fn har_records_what_loaded(page: Page) -> agito::Result<()> {
    let base = app().await;
    page.start_har();
    page.goto(&base).await?;
    page.locator("#load").click().await?;
    expect(&page.locator("#out")).to_have_text("server").await?;
    page.wait_for_network_idle(Duration::from_millis(200), None)
        .await?;
    let har = page.stop_har().expect("recording");
    let entries = har["log"]["entries"].as_array().unwrap();
    let api = entries
        .iter()
        .find(|e| {
            e["request"]["url"]
                .as_str()
                .is_some_and(|u| u.ends_with("/api/data"))
        })
        .expect("api call recorded");
    assert_eq!(api["response"]["status"], 200);
    assert_eq!(api["response"]["content"]["mimeType"], "application/json");
    assert!(page.stop_har().is_none());

    let path = std::env::temp_dir().join(format!("agito-har-{}.har", std::process::id()));
    page.start_har();
    page.reload().await?;
    page.stop_har_to(&path)?;
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(written["log"]["version"], "1.2");
    std::fs::remove_file(path).unwrap();
    Ok(())
}

#[agito::test]
async fn routes_reach_out_of_process_iframes(page: Page) -> agito::Result<()> {
    let base = serve(&[
        ("/", 200, r#"<iframe id="f"></iframe>
            <script>document.getElementById('f').src = 'http://localhost:' + location.port + '/inner';</script>"#),
        ("/inner", 200, APP),
        ("/api/data", 200, r#"{"name":"server"}"#),
    ])
    .await;
    page.route("**/api/data", |route| async move {
        let _ = route
            .fulfill(FulfillOptions::json(&json!({ "name": "mocked in iframe" })))
            .await;
    })
    .await?;
    page.goto(&base).await?;
    let frame = page.frame_locator("#f");
    frame.locator("#load").click().await?;
    expect(&frame.locator("#out"))
        .to_have_text("mocked in iframe")
        .await?;
    assert!(frame.frame().await?.is_out_of_process());
    Ok(())
}

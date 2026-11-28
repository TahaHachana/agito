//! Saving a context's storage state and starting another from it. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

use agito::storage::{SameSite, StorageState};
use agito::{Browser, BrowserContext, ContextOptions, Cookie, FulfillOptions, Page};

const APP: &str = r#"<!doctype html><title>app</title>
<iframe id="widget" src="https://widget.test/"></iframe>"#;

/// Serves `https://app.test`, which embeds a cross-origin iframe from `https://widget.test`.
async fn serve(context: &BrowserContext) -> agito::Result<()> {
    context
        .route("https://app.test/**", |route| async move {
            let _ = route.fulfill(FulfillOptions::html(APP)).await;
        })
        .await?;
    context
        .route("https://widget.test/**", |route| async move {
            let _ = route.fulfill(FulfillOptions::html("<p>widget</p>")).await;
        })
        .await?;
    Ok(())
}

/// The page's widget iframe, which is out of process and registers a moment after the page
/// loads.
async fn widget_frame(page: &Page) -> agito::Frame {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(frame) = page.frame_by_url("https://widget.test/") {
            return frame;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the widget frame never appeared"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn scratch_file(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("agito-storage-{name}-{}.json", std::process::id()))
}

#[agito::test]
async fn a_saved_state_starts_another_context_where_this_one_left_off(
    browser: Browser,
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    serve(&context).await?;
    page.goto("https://app.test/").await?;
    page.evaluate::<()>(
        "localStorage.setItem('token', 'xyz'), \
         document.cookie = 'sid=abc; Secure; SameSite=None; path=/'",
    )
    .await?;
    let widget = widget_frame(&page).await;
    widget
        .evaluate::<()>("localStorage.setItem('seen', 'yes')")
        .await?;
    let mut http_only = Cookie::new("auth", "secret", "https://app.test");
    http_only.http_only = true;
    http_only.secure = true;
    context.add_cookies(&[http_only]).await?;

    let state = context.storage_state().await?;
    // The widget's storage is partitioned under app.test, so it is not the storage widget.test
    // has on its own, and is left out.
    let origins: Vec<&str> = state.origins.iter().map(|o| o.origin.as_str()).collect();
    assert_eq!(origins, ["https://app.test"]);
    let sid = state.cookies.iter().find(|c| c.name == "sid").unwrap();
    assert_eq!(sid.same_site, Some(SameSite::None));
    assert_eq!(sid.expires, -1.0, "a session cookie");

    // Through a file, as a suite would keep it.
    let file = scratch_file("roundtrip");
    state.save(&file)?;
    let loaded = StorageState::load(&file)?;
    std::fs::remove_file(&file).unwrap();
    assert_eq!(loaded, state);

    let restored = browser
        .new_context(ContextOptions {
            storage_state: Some(loaded),
            ..Default::default()
        })
        .await?;
    assert!(
        restored.pages().is_empty(),
        "the page that wrote local storage is gone"
    );
    serve(&restored).await?;
    let again = restored.new_page().await?;
    again.goto("https://app.test/").await?;
    let token: Option<String> = again.evaluate("localStorage.getItem('token')").await?;
    assert_eq!(token.as_deref(), Some("xyz"));
    let cookie: String = again.evaluate("document.cookie").await?;
    assert!(cookie.contains("sid=abc"), "{cookie}");
    // The restored widget starts with empty storage of its own.
    let widget = widget_frame(&again).await;
    let seen: Option<String> = widget.evaluate("localStorage.getItem('seen')").await?;
    assert_eq!(seen, None);

    let round_trip = restored.storage_state().await?;
    let auth = round_trip
        .cookies
        .iter()
        .find(|c| c.name == "auth")
        .unwrap();
    assert!(auth.http_only && auth.secure);
    let sid = round_trip.cookies.iter().find(|c| c.name == "sid").unwrap();
    assert_eq!(sid.same_site, Some(SameSite::None));
    restored.close().await?;
    Ok(())
}

#[agito::test]
async fn a_state_with_only_cookies_opens_no_page(browser: Browser) -> agito::Result<()> {
    // Chrome caps a cookie's lifetime at 400 days, so expire well inside that.
    let expires = (std::time::SystemTime::now() + std::time::Duration::from_secs(30 * 86_400))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64;
    let state: StorageState = serde_json::from_value(serde_json::json!({
        "cookies": [{ "name": "theme", "value": "dark", "domain": "app.test", "path": "/",
                      "expires": expires, "httpOnly": false, "secure": false, "sameSite": "Lax" }]
    }))
    .unwrap();
    let context = browser
        .new_context(ContextOptions {
            storage_state: Some(state),
            ..Default::default()
        })
        .await?;
    let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = opened.clone();
    context.on_page(move |_| {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    });
    let cookies = context.cookies().await?;
    assert_eq!(cookies.len(), 1);
    assert_eq!(cookies[0].expires, Some(expires));
    assert!(context.pages().is_empty());
    assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 0);
    context.close().await?;
    Ok(())
}

#[agito::test]
async fn an_empty_context_has_an_empty_state(context: BrowserContext) -> agito::Result<()> {
    let state = context.storage_state().await?;
    assert_eq!(
        state,
        StorageState::default(),
        "about:blank keeps no storage"
    );
    Ok(())
}

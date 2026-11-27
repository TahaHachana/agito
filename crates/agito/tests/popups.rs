//! Popups and pages opened by pages. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agito::{BrowserContext, FulfillOptions, Page, expect};
use common::serve;

const OPENER: &str = r#"<button id="open" onclick="window.open('/child')">open</button>
<a id="blank" href="/child" target="_blank">new tab</a>
<button id="noopener" onclick="window.open('/child', '_blank', 'noopener')">noopener</button>
<button id="slow" onclick="window.open('/slow-child')">slow</button>"#;
const CHILD: &str = r#"<title>child</title><button id="b" onclick="this.textContent='clicked'">child button</button>
<button id="close" onclick="window.close()">close</button>
<p id="flag"></p><script>document.getElementById('flag').textContent = String(window.agitoFlag);</script>"#;

async fn site() -> String {
    serve(&[
        ("/", 200, OPENER),
        ("/child", 200, CHILD),
        ("/slow-child", 200, CHILD),
    ])
    .await
}

#[agito::test]
async fn window_open_creates_a_popup(context: BrowserContext, page: Page) -> agito::Result<()> {
    let base = site().await;
    page.goto(&base).await?;
    let popup = page.wait_for_popup(None);
    page.locator("#open").click().await?;
    let popup = popup.await?;
    popup
        .wait_for_load_state(agito::WaitUntil::Load, None)
        .await?;
    assert_eq!(popup.url(), format!("{base}/child"));
    assert_eq!(popup.title().await?, "child");
    assert_eq!(
        popup.opener().map(|o| o.target_id().to_owned()),
        Some(page.target_id().to_owned())
    );
    popup.locator("#b").click().await?;
    expect(&popup.locator("#b")).to_have_text("clicked").await?;
    assert_eq!(context.pages().len(), 2);
    Ok(())
}

#[agito::test]
async fn target_blank_links_open_popups(page: Page) -> agito::Result<()> {
    page.goto(&site().await).await?;
    let popup = page.wait_for_popup(None);
    page.locator("#blank").click().await?;
    let popup = popup.await?;
    expect(&popup).to_have_url("**/child").await?;
    expect(&popup.locator("#b")).to_be_visible().await?;
    Ok(())
}

#[agito::test]
async fn popups_get_context_routes_scripts_and_viewport_from_the_start(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    context
        .add_init_script("window.agitoFlag = 'from init script'")
        .await?;
    context
        .route("http://popup.test/**", |route| async move {
            let _ = route
                .fulfill(FulfillOptions::html("<h1>routed popup</h1>"))
                .await;
        })
        .await?;
    page.set_content(r#"<button onclick="window.open('http://popup.test/x')">open</button>"#)
        .await?;
    let popup = page.wait_for_popup(None);
    page.locator("button").click().await?;
    let popup = popup.await?;
    expect(&popup.locator("h1"))
        .to_have_text("routed popup")
        .await?;
    let flag: String = popup.evaluate("String(window.agitoFlag)").await?;
    assert_eq!(flag, "from init script");
    let width: u32 = popup.evaluate("window.innerWidth").await?;
    assert_eq!(width, context.options().viewport.unwrap().width);

    // The init script ran before the popup's own scripts.
    let base = site().await;
    let second = page.wait_for_popup(None);
    page.evaluate::<()>(&format!("window.open('{base}/child')"))
        .await?;
    let second = second.await?;
    expect(&second.locator("#flag"))
        .to_have_text("from init script")
        .await?;
    Ok(())
}

#[agito::test]
async fn handlers_see_new_pages_and_popups(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let sink = opened.clone();
    context.on_page(move |p| sink.lock().unwrap().push(p.target_id().to_owned()));
    let popups = Arc::new(Mutex::new(0));
    let counter = popups.clone();
    page.on_popup(move |_| *counter.lock().unwrap() += 1);

    let made = context.new_page().await?;
    page.goto(&site().await).await?;
    let next = context.wait_for_page(None);
    page.locator("#open").click().await?;
    let popup = next.await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while *popups.lock().unwrap() == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let opened = opened.lock().unwrap().clone();
    assert!(opened.contains(&made.target_id().to_owned()), "{opened:?}");
    assert!(opened.contains(&popup.target_id().to_owned()), "{opened:?}");
    assert_eq!(*popups.lock().unwrap(), 1);
    Ok(())
}

#[agito::test]
async fn popups_that_close_themselves_are_closed(page: Page) -> agito::Result<()> {
    page.goto(&site().await).await?;
    let popup = page.wait_for_popup(None);
    page.locator("#open").click().await?;
    let popup = popup.await?;
    popup.locator("#close").click().await?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !popup.is_closed() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(popup.is_closed());
    assert!(popup.title().await.unwrap_err().is_browser_lost());
    Ok(())
}

/// A command still waiting for its answer when its page goes away fails at once, rather than
/// waiting out the command timeout: Chrome does not always answer commands on a target it is
/// destroying.
#[agito::test]
async fn commands_pending_on_a_page_that_closes_fail_at_once(page: Page) -> agito::Result<()> {
    page.goto(&site().await).await?;
    let popup = page.wait_for_popup(None);
    page.locator("#open").click().await?;
    let popup = popup.await?;
    let started = Instant::now();
    let pending = popup.evaluate::<()>("new Promise(() => setTimeout(() => window.close(), 100))");
    let err = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .expect("the command fails when the page closes, not at the command timeout")
        .unwrap_err();
    assert!(err.is_browser_lost(), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    Ok(())
}

#[agito::test]
async fn noopener_pages_still_reach_the_context(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    page.goto(&site().await).await?;
    let next = context.wait_for_page(None);
    page.locator("#noopener").click().await?;
    let opened = next.await?;
    expect(&opened).to_have_url("**/child").await?;
    expect(&opened.locator("#b")).to_be_visible().await?;
    Ok(())
}

#[agito::test]
async fn waiting_for_a_popup_that_never_opens_times_out(page: Page) -> agito::Result<()> {
    page.set_content("<p>nothing opens</p>").await?;
    let err = page
        .wait_for_popup(Some(Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert!(err.is_timeout());
    Ok(())
}

#[agito::test]
async fn popup_load_state_waits_for_its_document(page: Page) -> agito::Result<()> {
    page.goto(&site().await).await?;
    let popup = page.wait_for_popup(None);
    page.locator("#slow").click().await?;
    let popup = popup.await?;
    popup
        .wait_for_load_state(agito::WaitUntil::Load, None)
        .await?;
    assert!(popup.url().ends_with("/slow-child"), "{}", popup.url());
    let ready: String = popup.evaluate("document.readyState").await?;
    assert_eq!(ready, "complete");
    Ok(())
}

//! Performance metrics, budgets and their assertions. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use std::time::Duration;

use agito::performance::PerformanceBudget;
use agito::{Page, expect};
use common::serve;

const STEADY: &str = "<!doctype html><title>Steady</title><main><h1>Hello</h1><p>Text.</p></main>";

/// A banner pushed in above the text after it has rendered: a layout shift.
const SHIFTING: &str = "<!doctype html><title>Shifting</title><body style='margin:0'>\
    <p id=text style='font-size:40px;margin:0'>Read me</p>\
    <script>setTimeout(() => { const b = document.createElement('div'); b.id = 'banner'; \
    b.style.height = '300px'; document.body.prepend(b); }, 150)</script></body>";

/// A button whose handler blocks the page for 200 ms: a slow interaction.
const SLOW: &str = "<!doctype html><title>Slow</title><button id=b \
    onclick=\"const end = performance.now() + 200; while (performance.now() < end) {}\">Go</button>";

#[agito::test]
async fn a_loaded_page_reports_its_vitals(page: Page) -> agito::Result<()> {
    let base = serve(&[("/", 200, STEADY)]).await;
    page.goto(&base).await?;
    expect(&page).to_have_lcp_below(10_000.0).await?;
    let metrics = page.performance_metrics().await?;
    assert!(metrics.fcp.is_some_and(|v| v > 0.0), "{metrics:?}");
    assert!(
        metrics.lcp.is_some_and(|v| v >= metrics.fcp.unwrap()),
        "{metrics:?}"
    );
    assert!(metrics.ttfb.is_some_and(|v| v >= 0.0), "{metrics:?}");
    assert_eq!(metrics.cls, Some(0.0), "nothing moved");
    assert!(metrics.layout_shifts.is_empty());
    assert_eq!(metrics.inp, None, "nothing was clicked");
    assert!(metrics.dom_nodes.is_some_and(|n| n > 3), "{metrics:?}");
    assert!(metrics.js_heap_size.is_some_and(|n| n > 0), "{metrics:?}");
    Ok(())
}

#[agito::test]
async fn layout_shifts_name_what_moved(page: Page) -> agito::Result<()> {
    let base = serve(&[("/", 200, SHIFTING)]).await;
    page.goto(&base).await?;
    expect(&page.locator("#banner")).to_be_attached().await?;
    // The shift is reported once the frame that shows it has rendered.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let metrics = loop {
        let metrics = page.performance_metrics().await?;
        if !metrics.layout_shifts.is_empty() || std::time::Instant::now() > deadline {
            break metrics;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(metrics.cls.is_some_and(|v| v > 0.0), "{metrics:?}");
    let shift = &metrics.layout_shifts[0];
    assert!(shift.score > 0.0 && shift.time > 0.0, "{shift:?}");
    assert!(
        shift.sources.iter().any(|s| s.ends_with("#text")),
        "the paragraph moved: {shift:?}"
    );

    let strict = PerformanceBudget {
        cls: Some(0.001),
        ..PerformanceBudget::none()
    };
    let err = expect(&page)
        .with_timeout(Duration::from_millis(300))
        .to_meet_performance_budget_with(&strict)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("to_meet_performance_budget"), "{err}");
    assert!(err.contains("CLS: "), "{err}");
    assert!(err.contains("over by"), "{err}");
    expect(&page)
        .not()
        .with_timeout(Duration::from_millis(300))
        .to_meet_performance_budget_with(&strict)
        .await?;
    Ok(())
}

#[agito::test]
async fn a_slow_interaction_shows_in_inp(page: Page) -> agito::Result<()> {
    let base = serve(&[("/", 200, SLOW)]).await;
    page.goto(&base).await?;
    let err = expect(&page)
        .with_timeout(Duration::from_millis(200))
        .to_have_inp_below(1000.0)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("INP not measured"), "{err}");

    // Neither way passes on a metric not measured yet.
    let err = expect(&page)
        .not()
        .with_timeout(Duration::from_millis(200))
        .to_have_inp_below(1000.0)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("INP not measured"), "{err}");

    page.locator("#b").click().await?;
    // A click is several events, the slow one reported once its next frame has painted, so
    // INP rises as they come in: wait for it to pass the handler's 200 ms.
    expect(&page).not().to_have_inp_below(199.0).await?;
    expect(&page).to_have_inp_below(5000.0).await?;
    Ok(())
}

#[agito::test(perf_budget(lcp = 10000, cls = 0.25, dom_nodes = 5000))]
async fn a_test_s_budget_is_the_one_in_force(page: Page) -> agito::Result<()> {
    let base = serve(&[("/", 200, STEADY)]).await;
    page.goto(&base).await?;
    expect(&page).to_meet_performance_budget().await?;
    Ok(())
}

#[agito::test]
async fn without_a_budget_there_is_nothing_to_meet(page: Page) -> agito::Result<()> {
    page.set_content(STEADY).await?;
    let err = expect(&page)
        .to_meet_performance_budget()
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("no performance budget is in force"), "{err}");
    Ok(())
}

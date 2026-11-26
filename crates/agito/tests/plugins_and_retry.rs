//! Plugin registration and retry-on-browser-loss. Run with `--features integration`.
#![cfg(feature = "integration")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agito::handles::ElementHandle;
use agito::plugin::{ActionResult, LifecycleHook, Plugin, PluginContext};
use agito::selectors::{CssStrategy, SelectorStrategy};
use agito::{Browser, Page, expect};
use async_trait::async_trait;

/// `id=foo` selects `#foo`, built on the CSS strategy.
struct IdStrategy;

#[async_trait]
impl SelectorStrategy for IdStrategy {
    fn name(&self) -> &str {
        "id"
    }
    async fn resolve(
        &self,
        selector: &str,
        page: &Page,
        pierce: bool,
    ) -> agito::Result<Vec<ElementHandle>> {
        CssStrategy
            .resolve(&format!("[id=\"{selector}\"]"), page, pierce)
            .await
    }
}

static ACTIONS: AtomicUsize = AtomicUsize::new(0);

struct CountActions;

#[async_trait]
impl LifecycleHook for CountActions {
    async fn after_action(&self, _page: &Page, _result: &ActionResult) -> agito::Result<()> {
        ACTIONS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Default)]
#[agito::plugin]
struct TestPlugin;

#[async_trait]
impl Plugin for TestPlugin {
    fn id(&self) -> &str {
        "tests.id-selector"
    }
    async fn on_loaded(&self, context: &mut PluginContext) -> agito::Result<()> {
        context.register_selector_strategy(Arc::new(IdStrategy));
        context.register_lifecycle_hook(Arc::new(CountActions));
        Ok(())
    }
}

#[agito::test]
async fn discovered_plugins_register_strategies_and_hooks(page: Page) -> agito::Result<()> {
    page.set_content("<button id='go' onclick=\"this.textContent='gone'\">go</button>")
        .await?;
    let before = ACTIONS.load(Ordering::SeqCst);
    page.locator("id=go").click().await?;
    expect(&page.locator("id=go")).to_have_text("gone").await?;
    assert!(ACTIONS.load(Ordering::SeqCst) > before);
    Ok(())
}

static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

#[agito::test(retries = 1)]
async fn a_lost_browser_is_relaunched_and_the_test_rerun(
    browser: Browser,
    page: Page,
) -> agito::Result<()> {
    if ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
        browser.close().await?;
    }
    page.set_content("<p>alive</p>").await?;
    expect(&page.locator("p")).to_have_text("alive").await?;
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 2);
    Ok(())
}

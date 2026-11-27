//! The plugin example in the agito book (`book/src/extending.md`). A plugin registered with
//! `#[agito::plugin]` loads into every context of the test binary it is linked into, so it has
//! a binary of its own.
#![cfg(feature = "integration")]

use std::sync::{Arc, Mutex};

use agito::frame::Frame;
use agito::handles::ElementHandle;
use agito::plugin::{LifecycleHook, Plugin, PluginContext};
use agito::selectors::SelectorStrategy;
use agito::{Page, Response, expect};
use async_trait::async_trait;

// ANCHOR: strategy
/// `qa=checkout` finds elements whose `data-qa` attribute is `checkout`.
struct QaStrategy;

#[async_trait]
impl SelectorStrategy for QaStrategy {
    fn name(&self) -> &str {
        "qa"
    }

    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        _pierce_shadow: bool,
    ) -> agito::Result<Vec<ElementHandle>> {
        let value = serde_json::to_string(selector).expect("strings serialize");
        let expression = format!(
            "[...document.querySelectorAll('[data-qa]')].filter(e => e.dataset.qa === {value})"
        );
        frame.query_all(&expression).await
    }
}
// ANCHOR_END: strategy

// ANCHOR: hook
/// Records every URL a page navigates to.
#[derive(Default)]
struct NavigationLog {
    urls: Mutex<Vec<String>>,
}

#[async_trait]
impl LifecycleHook for NavigationLog {
    async fn after_navigation(
        &self,
        page: &Page,
        _response: Option<&Response>,
    ) -> agito::Result<()> {
        self.urls.lock().unwrap().push(page.url());
        Ok(())
    }
}

/// The log the plugin registers, kept where the tests can read it.
static LOG: std::sync::LazyLock<Arc<NavigationLog>> = std::sync::LazyLock::new(Default::default);
// ANCHOR_END: hook

// ANCHOR: plugin
#[agito::plugin]
#[derive(Default)]
struct QaPlugin;

#[async_trait]
impl Plugin for QaPlugin {
    fn id(&self) -> &str {
        "example.qa"
    }

    async fn on_loaded(&self, context: &mut PluginContext) -> agito::Result<()> {
        context.register_selector_strategy(Arc::new(QaStrategy));
        context.register_lifecycle_hook(LOG.clone());
        Ok(())
    }
}
// ANCHOR_END: plugin

// ANCHOR: use
#[agito::test]
async fn the_qa_selector_finds_the_checkout_button(page: Page) -> agito::Result<()> {
    page.goto("data:text/html,<button data-qa=checkout>Pay now</button>")
        .await?;
    expect(&page.locator("qa=checkout"))
        .to_have_text("Pay now")
        .await?;
    assert!(
        LOG.urls
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.starts_with("data:text/html"))
    );
    Ok(())
}
// ANCHOR_END: use

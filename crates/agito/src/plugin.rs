//! The plugin API. Built-in selector strategies are registered through it exactly as a
//! third-party plugin would register its own.
//!
//! Plugins come from [`LaunchOptions::plugins`](crate::LaunchOptions::plugins) and from types
//! marked `#[agito::plugin]`, which are collected at link time.

use std::sync::Arc;

use async_trait::async_trait;

use crate::accessibility::AccessibilityRule;
use crate::error::Result;
use crate::page::{Page, Response};
use crate::selectors::{SelectorRegistry, SelectorStrategy, register_builtins};

/// A plugin. `on_loaded` runs once per browser context, and is where registration happens.
///
/// Register one with `#[agito::plugin]` (it must implement `Default`) or pass it in
/// [`LaunchOptions::plugins`](crate::LaunchOptions::plugins). Plugin traits are async traits,
/// so implementing one needs the `async-trait` crate:
///
/// ```no_run
/// use std::sync::Arc;
/// use agito::plugin::{Plugin, PluginContext};
/// use agito::selectors::TestIdStrategy;
///
/// #[agito::plugin]
/// #[derive(Default)]
/// struct QaIds;
///
/// #[async_trait::async_trait]
/// impl Plugin for QaIds {
///     fn id(&self) -> &str {
///         "example.qa-ids"
///     }
///
///     async fn on_loaded(&self, context: &mut PluginContext) -> agito::Result<()> {
///         // `data-qa=checkout` finds elements whose `data-qa` attribute is `checkout`.
///         context.register_selector_strategy(Arc::new(TestIdStrategy::new("data-qa")));
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// A stable, unique id. Duplicates are skipped (first wins); built-in ids are reserved.
    fn id(&self) -> &str;

    /// A name for messages; the id by default.
    fn name(&self) -> &str {
        self.id()
    }

    /// Registers what the plugin provides. Runs once for every new browser context.
    async fn on_loaded(&self, context: &mut PluginContext) -> Result<()>;

    /// Runs when the context closes. Does nothing by default.
    async fn on_unloaded(&self) -> Result<()> {
        Ok(())
    }
}

/// What happened in an action, for [`LifecycleHook::after_action`].
#[derive(Debug, Clone)]
pub struct ActionResult {
    /// The action's name, such as `click`.
    pub action: String,
    /// The error message when the action failed.
    pub error: Option<String>,
}

/// Callbacks around navigations, actions and page lifetimes. Every method defaults to doing
/// nothing. Errors are logged and otherwise ignored: a hook never fails the test.
#[async_trait]
pub trait LifecycleHook: Send + Sync + 'static {
    /// Before `goto` navigates to `url`.
    async fn before_navigation(&self, _page: &Page, _url: &str) -> Result<()> {
        Ok(())
    }
    /// After `goto` finishes, with the document's response.
    async fn after_navigation(&self, _page: &Page, _response: Option<&Response>) -> Result<()> {
        Ok(())
    }
    /// Before a locator action, such as `click`.
    async fn before_action(&self, _page: &Page, _action: &str) -> Result<()> {
        Ok(())
    }
    /// After a locator action, with whether it failed.
    async fn after_action(&self, _page: &Page, _result: &ActionResult) -> Result<()> {
        Ok(())
    }
    /// When a page opens in the context, popups included.
    async fn on_page_created(&self, _page: &Page) -> Result<()> {
        Ok(())
    }
    /// When `page.close()` closes a page.
    async fn on_page_closed(&self, _page: &Page) -> Result<()> {
        Ok(())
    }
}

/// What a plugin can register.
pub struct PluginContext {
    selectors: SelectorRegistry,
    hooks: Vec<Arc<dyn LifecycleHook>>,
    accessibility_rules: Vec<Arc<dyn AccessibilityRule>>,
}

impl PluginContext {
    /// Adds a selector strategy, available by its name as a selector prefix.
    pub fn register_selector_strategy(&mut self, strategy: Arc<dyn SelectorStrategy>) {
        self.selectors.register(strategy);
    }

    /// Adds a lifecycle hook.
    pub fn register_lifecycle_hook(&mut self, hook: Arc<dyn LifecycleHook>) {
        self.hooks.push(hook);
    }

    /// Adds an accessibility rule that every audit runs, replacing a rule with the same id, so
    /// a plugin can stand in for a built-in rule.
    pub fn register_accessibility_rule(&mut self, rule: Arc<dyn AccessibilityRule>) {
        self.accessibility_rules.retain(|r| r.id() != rule.id());
        self.accessibility_rules.push(rule);
    }
}

/// A plugin registered with `#[agito::plugin]`.
pub struct PluginRegistration {
    /// Makes the plugin.
    pub factory: fn() -> Arc<dyn Plugin>,
}

inventory::collect!(PluginRegistration);

struct BuiltinSelectors;

const BUILTIN_SELECTORS_ID: &str = "agito.builtin.selectors";

#[async_trait]
impl Plugin for BuiltinSelectors {
    fn id(&self) -> &str {
        BUILTIN_SELECTORS_ID
    }
    async fn on_loaded(&self, context: &mut PluginContext) -> Result<()> {
        register_builtins(&context.selectors);
        Ok(())
    }
}

/// The built-in accessibility rules, registered as a plugin would register its own.
struct BuiltinAccessibilityRules;

const BUILTIN_ACCESSIBILITY_ID: &str = "agito.builtin.accessibility-rules";

#[async_trait]
impl Plugin for BuiltinAccessibilityRules {
    fn id(&self) -> &str {
        BUILTIN_ACCESSIBILITY_ID
    }
    async fn on_loaded(&self, context: &mut PluginContext) -> Result<()> {
        for rule in crate::accessibility::rules::builtin() {
            context.register_accessibility_rule(rule);
        }
        Ok(())
    }
}

/// The plugins loaded into one context.
pub(crate) struct PluginHost {
    loaded: Vec<Arc<dyn Plugin>>,
    selectors: SelectorRegistry,
    hooks: Vec<Arc<dyn LifecycleHook>>,
    accessibility_rules: Vec<Arc<dyn AccessibilityRule>>,
}

impl PluginHost {
    pub async fn load(configured: &[Arc<dyn Plugin>]) -> Result<Self> {
        let mut context = PluginContext {
            selectors: SelectorRegistry::default(),
            hooks: Vec::new(),
            accessibility_rules: Vec::new(),
        };
        let builtins: [Arc<dyn Plugin>; 2] = [
            Arc::new(BuiltinSelectors),
            Arc::new(BuiltinAccessibilityRules),
        ];
        // A failing built-in is a bug in agito, so it propagates.
        for builtin in &builtins {
            builtin.on_loaded(&mut context).await?;
        }
        let mut loaded = builtins.to_vec();
        let mut seen = std::collections::HashSet::from([
            BUILTIN_SELECTORS_ID.to_owned(),
            BUILTIN_ACCESSIBILITY_ID.to_owned(),
        ]);
        let discovered = inventory::iter::<PluginRegistration>
            .into_iter()
            .map(|r| (r.factory)());
        for plugin in configured.iter().cloned().chain(discovered) {
            if !seen.insert(plugin.id().to_owned()) {
                continue;
            }
            match plugin.on_loaded(&mut context).await {
                Ok(()) => loaded.push(plugin),
                Err(e) => {
                    tracing::warn!(plugin = plugin.id(), error = %e, "plugin failed to load; skipping it")
                }
            }
        }
        Ok(Self {
            loaded,
            selectors: context.selectors,
            hooks: context.hooks,
            accessibility_rules: context.accessibility_rules,
        })
    }

    pub fn selectors(&self) -> SelectorRegistry {
        self.selectors.clone()
    }

    pub fn hooks(&self) -> Vec<Arc<dyn LifecycleHook>> {
        self.hooks.clone()
    }

    pub fn accessibility_rules(&self) -> Vec<Arc<dyn AccessibilityRule>> {
        self.accessibility_rules.clone()
    }

    pub async fn unload(&self) {
        for plugin in self.loaded.iter().rev() {
            if let Err(e) = plugin.on_unloaded().await {
                tracing::warn!(plugin = plugin.id(), error = %e, "plugin failed to unload");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting(&'static str, Arc<AtomicUsize>);

    #[async_trait]
    impl Plugin for Counting {
        fn id(&self) -> &str {
            self.0
        }
        async fn on_loaded(&self, _context: &mut PluginContext) -> Result<()> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct Failing;

    #[async_trait]
    impl Plugin for Failing {
        fn id(&self) -> &str {
            "failing"
        }
        async fn on_loaded(&self, _context: &mut PluginContext) -> Result<()> {
            Err(crate::Error::other("nope"))
        }
    }

    #[tokio::test]
    async fn builtins_load_first_and_duplicates_are_skipped() {
        let count = Arc::new(AtomicUsize::new(0));
        let plugins: Vec<Arc<dyn Plugin>> = vec![
            Arc::new(Counting("p", count.clone())),
            Arc::new(Counting("p", count.clone())),
            Arc::new(Counting(BUILTIN_SELECTORS_ID, count.clone())),
            Arc::new(Counting(BUILTIN_ACCESSIBILITY_ID, count.clone())),
            Arc::new(Failing),
        ];
        let host = PluginHost::load(&plugins).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert!(host.selectors().get("css").is_some());
        assert!(host.selectors().get("ROLE").is_some());
        // The two built-ins and `p`; the second `p`, the reserved id and the failure are not.
        assert_eq!(host.loaded.len(), 3);
        assert_eq!(host.accessibility_rules().len(), 9);
        assert_eq!(host.selectors().by_priority()[0].name(), "data-testid");
    }
}

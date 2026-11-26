//! Browser contexts: isolated sessions (cookies, storage, cache) holding pages.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agito_protocol::cdp::{network, storage, target};
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::config;
use crate::error::Result;
use crate::page::{Page, PageInner};
use crate::plugin::{LifecycleHook, PluginHost};
use crate::selectors::SelectorRegistry;

/// Viewport dimensions in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

/// Light or dark preference reported to `prefers-color-scheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
    NoPreference,
}

impl ColorScheme {
    fn as_css(self) -> &'static str {
        match self {
            ColorScheme::Light => "light",
            ColorScheme::Dark => "dark",
            ColorScheme::NoPreference => "no-preference",
        }
    }
}

/// How to set up a context. [`Default`] applies `agito.config.json` and `AGITO_*` variables.
#[derive(Debug, Clone)]
pub struct ContextOptions {
    /// `None` leaves the window's own size; the default is 1280x720.
    pub viewport: Option<Viewport>,
    pub user_agent: Option<String>,
    pub locale: Option<String>,
    pub timezone_id: Option<String>,
    pub color_scheme: Option<ColorScheme>,
    pub ignore_https_errors: bool,
    /// Relative URLs passed to `goto` resolve against this.
    pub base_url: Option<String>,
    pub extra_http_headers: HashMap<String, String>,
    pub proxy_server: Option<String>,
    /// Default timeout for locator actions.
    pub action_timeout: Duration,
    /// Default timeout for navigations.
    pub navigation_timeout: Duration,
}

impl Default for ContextOptions {
    fn default() -> Self {
        let c = config::get();
        let viewport = c.context.viewport.as_ref();
        Self {
            viewport: Some(Viewport {
                width: viewport.and_then(|v| v.width).unwrap_or(1280),
                height: viewport.and_then(|v| v.height).unwrap_or(720),
            }),
            user_agent: None,
            locale: c.context.locale.clone(),
            timezone_id: None,
            color_scheme: c.context.color_scheme.as_deref().and_then(|s| match s {
                "light" => Some(ColorScheme::Light),
                "dark" => Some(ColorScheme::Dark),
                "no-preference" => Some(ColorScheme::NoPreference),
                _ => None,
            }),
            ignore_https_errors: c.context.ignore_https_errors.unwrap_or(false),
            base_url: c.context.base_url.clone(),
            extra_http_headers: HashMap::new(),
            proxy_server: None,
            action_timeout: Duration::from_millis(c.locator.timeout.unwrap_or(30_000)),
            navigation_timeout: Duration::from_secs(30),
        }
    }
}

impl ContextOptions {
    pub(crate) fn color_scheme_css(&self) -> Option<&'static str> {
        self.color_scheme.map(ColorScheme::as_css)
    }
}

/// A cookie, as read from or written to a context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Seconds since the epoch; `None` for a session cookie.
    pub expires: Option<f64>,
    pub http_only: bool,
    pub secure: bool,
    /// A URL to scope the cookie by, instead of `domain` and `path`.
    pub url: Option<String>,
}

impl Cookie {
    /// A cookie scoped to `url`.
    pub fn new(name: impl Into<String>, value: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            domain: None,
            path: None,
            expires: None,
            http_only: false,
            secure: false,
            url: Some(url.into()),
        }
    }
}

/// An isolated browser session. Cheap to clone.
#[derive(Clone)]
pub struct BrowserContext {
    pub(crate) inner: Arc<ContextInner>,
}

pub(crate) struct ContextInner {
    pub browser: Browser,
    pub id: String,
    pub options: ContextOptions,
    pub selectors: SelectorRegistry,
    pub hooks: Vec<Arc<dyn LifecycleHook>>,
    plugin_host: PluginHost,
    pub init_scripts: Mutex<Vec<String>>,
    pages: Mutex<Vec<Weak<PageInner>>>,
    closed: AtomicBool,
}

impl BrowserContext {
    pub(crate) fn from_inner(inner: Arc<ContextInner>) -> Self {
        Self { inner }
    }

    pub(crate) async fn create(
        browser: Browser,
        id: String,
        options: ContextOptions,
    ) -> Result<Self> {
        let host = PluginHost::load(&browser.inner.plugins).await?;
        let inner = Arc::new(ContextInner {
            browser,
            id,
            options,
            selectors: host.selectors(),
            hooks: host.hooks(),
            plugin_host: host,
            init_scripts: Mutex::new(Vec::new()),
            pages: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        });
        Ok(Self { inner })
    }

    /// The browser this context lives in.
    pub fn browser(&self) -> &Browser {
        &self.inner.browser
    }

    /// The protocol's id for this context.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn options(&self) -> &ContextOptions {
        &self.inner.options
    }

    /// Opens a new page in this context.
    pub async fn new_page(&self) -> Result<Page> {
        let browser_session = self.inner.browser.browser_session();
        let created = browser_session
            .send(&target::CreateTargetParams {
                url: "about:blank".into(),
                browser_context_id: Some(self.inner.id.clone()),
                ..Default::default()
            })
            .await?;
        let attached = browser_session
            .send(&target::AttachToTargetParams {
                target_id: created.target_id.clone(),
                flatten: Some(true),
            })
            .await;
        let attached = match attached {
            Ok(a) => a,
            Err(e) => {
                let _ = browser_session
                    .send(&target::CloseTargetParams {
                        target_id: created.target_id,
                    })
                    .await;
                return Err(e);
            }
        };
        let session = crate::transport::SessionRegistry::create_session(
            self.inner.browser.registry().as_ref(),
            &attached.session_id,
        );
        let page = match Page::initialize(self.clone(), session, created.target_id.clone()).await {
            Ok(page) => page,
            Err(e) => {
                crate::transport::SessionRegistry::remove_session(
                    self.inner.browser.registry().as_ref(),
                    &attached.session_id,
                );
                let _ = browser_session
                    .send(&target::CloseTargetParams {
                        target_id: created.target_id,
                    })
                    .await;
                return Err(e);
            }
        };
        self.inner
            .browser
            .register_page(&attached.session_id, &page.inner);
        self.inner
            .pages
            .lock()
            .unwrap()
            .push(Arc::downgrade(&page.inner));
        for hook in &self.inner.hooks {
            if let Err(e) = hook.on_page_created(&page).await {
                tracing::warn!(error = %e, "lifecycle hook on_page_created failed");
            }
        }
        Ok(page)
    }

    /// The pages still open in this context.
    pub fn pages(&self) -> Vec<Page> {
        let mut pages = self.inner.pages.lock().unwrap();
        pages.retain(|p| p.upgrade().is_some_and(|p| !p.is_closed()));
        pages
            .iter()
            .filter_map(|p| p.upgrade())
            .map(Page::from_inner)
            .collect()
    }

    /// Adds a script that runs in every page of this context before the page's own scripts,
    /// including pages already open (from their next navigation).
    pub async fn add_init_script(&self, script: impl Into<String>) -> Result<()> {
        let script = script.into();
        self.inner.init_scripts.lock().unwrap().push(script.clone());
        for page in self.pages() {
            page.add_init_script(script.clone()).await?;
        }
        Ok(())
    }

    /// Every cookie in this context.
    pub async fn cookies(&self) -> Result<Vec<Cookie>> {
        let result = self
            .inner
            .browser
            .browser_session()
            .send(&storage::GetCookiesParams {
                browser_context_id: Some(self.inner.id.clone()),
            })
            .await?;
        Ok(result
            .cookies
            .into_iter()
            .map(|c| Cookie {
                name: c.name,
                value: c.value,
                domain: Some(c.domain),
                path: Some(c.path),
                expires: (!c.session).then_some(c.expires),
                http_only: c.http_only,
                secure: c.secure,
                url: None,
            })
            .collect())
    }

    /// Adds cookies. Each needs either a `url` or a `domain` and `path`.
    pub async fn add_cookies(&self, cookies: &[Cookie]) -> Result<()> {
        let params = cookies
            .iter()
            .map(|c| network::CookieParam {
                name: c.name.clone(),
                value: c.value.clone(),
                url: c.url.clone(),
                domain: c.domain.clone(),
                path: c.path.clone(),
                secure: Some(c.secure),
                http_only: Some(c.http_only),
                expires: c.expires,
                ..Default::default()
            })
            .collect();
        self.inner
            .browser
            .browser_session()
            .send(&storage::SetCookiesParams {
                cookies: params,
                browser_context_id: Some(self.inner.id.clone()),
            })
            .await?;
        Ok(())
    }

    /// Removes every cookie in this context.
    pub async fn clear_cookies(&self) -> Result<()> {
        self.inner
            .browser
            .browser_session()
            .send(&storage::ClearCookiesParams {
                browser_context_id: Some(self.inner.id.clone()),
            })
            .await?;
        Ok(())
    }

    /// Closes every page in the context and the context itself.
    pub async fn close(&self) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        for page in self.pages() {
            page.inner.mark_closed();
        }
        let result = if self.inner.browser.inner.connection.is_closed() {
            Ok(())
        } else {
            self.inner
                .browser
                .browser_session()
                .send(&target::DisposeBrowserContextParams {
                    browser_context_id: self.inner.id.clone(),
                })
                .await
                .map(|_| ())
        };
        self.inner.plugin_host.unload().await;
        result
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for BrowserContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserContext")
            .field("id", &self.inner.id)
            .finish()
    }
}

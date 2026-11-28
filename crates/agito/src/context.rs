//! Browser contexts: isolated sessions (cookies, storage, cache) holding pages.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agito_protocol::cdp::{browser as cdp_browser, network, runtime, storage, target};
use tokio::sync::{broadcast, oneshot};

use crate::transport::SessionRegistry;
use serde::{Deserialize, Serialize};

use crate::browser::Browser;
use crate::config;
use crate::error::{Error, ErrorKind, Result};
use crate::files::Download;
use crate::page::{Page, PageInner};
use crate::plugin::{LifecycleHook, PluginHost};
use crate::selectors::SelectorRegistry;

/// The page's origin and local storage, or `null` for an opaque origin.
const READ_LOCAL_STORAGE: &str = "(() => { \
    if (location.origin === 'null') return null; \
    return [location.origin, Object.entries(localStorage)]; })()";

/// Writes `[name, value]` pairs to local storage.
const WRITE_LOCAL_STORAGE: &str =
    "(entries) => { for (const [name, value] of entries) localStorage.setItem(name, value); }";

fn same_site_from_cdp(same_site: network::CookieSameSite) -> Option<crate::storage::SameSite> {
    use crate::storage::SameSite;
    match same_site {
        network::CookieSameSite::Strict => Some(SameSite::Strict),
        network::CookieSameSite::Lax => Some(SameSite::Lax),
        network::CookieSameSite::None => Some(SameSite::None),
        _ => None,
    }
}

fn same_site_to_cdp(same_site: crate::storage::SameSite) -> network::CookieSameSite {
    use crate::storage::SameSite;
    match same_site {
        SameSite::Strict => network::CookieSameSite::Strict,
        SameSite::Lax => network::CookieSameSite::Lax,
        SameSite::None => network::CookieSameSite::None,
    }
}

/// Viewport dimensions in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    /// Width in CSS pixels.
    pub width: u32,
    /// Height in CSS pixels.
    pub height: u32,
}

/// Light or dark preference reported to `prefers-color-scheme`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    /// Reports `prefers-color-scheme: light`.
    Light,
    /// Reports `prefers-color-scheme: dark`.
    Dark,
    /// Reports `prefers-color-scheme: no-preference`.
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
    /// The `User-Agent` header and `navigator.userAgent`; the browser's own when `None`.
    pub user_agent: Option<String>,
    /// The locale, such as `fr-FR`: `navigator.language` and `Accept-Language`.
    pub locale: Option<String>,
    /// The timezone, such as `Europe/Paris`; the machine's when `None`.
    pub timezone_id: Option<String>,
    /// What `prefers-color-scheme` reports.
    pub color_scheme: Option<ColorScheme>,
    /// Accept invalid TLS certificates, for test servers.
    pub ignore_https_errors: bool,
    /// Relative URLs passed to `goto` resolve against this.
    pub base_url: Option<String>,
    /// Headers added to every request the context makes.
    pub extra_http_headers: HashMap<String, String>,
    /// A proxy for the context's requests, such as `http://proxy:3128`.
    pub proxy_server: Option<String>,
    /// Default timeout for locator actions.
    pub action_timeout: Duration,
    /// Default timeout for navigations.
    pub navigation_timeout: Duration,
    /// Cookies and local storage to start with, such as a signed-in session saved with
    /// [`BrowserContext::storage_state`].
    pub storage_state: Option<crate::storage::StorageState>,
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
            storage_state: None,
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
    /// The cookie's name.
    pub name: String,
    /// The cookie's value.
    pub value: String,
    /// The domain it is sent to.
    pub domain: Option<String>,
    /// The path it is sent for.
    pub path: Option<String>,
    /// Seconds since the epoch; `None` for a session cookie.
    pub expires: Option<f64>,
    /// Hidden from page scripts (`document.cookie`).
    pub http_only: bool,
    /// Sent over HTTPS only.
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

/// An isolated browser session: its own cookies, storage, cache, routes and init scripts, with
/// its own [`ContextOptions`] for every page in it. Cheap to clone.
///
/// ```no_run
/// # async fn example(context: agito::BrowserContext) -> agito::Result<()> {
/// use agito::Cookie;
///
/// context.add_cookies(&[Cookie::new("session", "abc123", "https://shop.test")]).await?;
/// let page = context.new_page().await?;
/// let other = context.new_page().await?; // shares the cookie
/// # Ok(()) }
/// ```
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
    pub routes: Mutex<Vec<crate::network::RouteEntry>>,
    pages: Mutex<Vec<Weak<PageInner>>>,
    closed: AtomicBool,
    /// Held while `new_page` creates a target, so the attach that follows knows it is claimed.
    creating: tokio::sync::Mutex<()>,
    /// Targets `new_page` created and is waiting for, by target id.
    claims: Mutex<HashMap<String, oneshot::Sender<Result<Page>>>>,
    page_events: broadcast::Sender<Page>,
    page_handlers: Mutex<Vec<PageHandler>>,
    /// Where this context's downloads land, named by download id; removed with the context.
    pub download_dir: PathBuf,
    pub downloads: Mutex<HashMap<String, Download>>,
}

impl Drop for ContextInner {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.download_dir);
    }
}

type PageHandler = Arc<dyn Fn(&Page) + Send + Sync>;

/// How long `new_page` waits for its target to attach.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(30);

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
        let download_dir = crate::files::temp_dir("downloads")?;
        let behavior = browser
            .browser_session()
            .send(&cdp_browser::SetDownloadBehaviorParams {
                behavior: cdp_browser::SetDownloadBehaviorParamsBehavior::AllowAndName,
                browser_context_id: Some(id.clone()),
                download_path: Some(download_dir.to_string_lossy().into_owned()),
                events_enabled: Some(true),
            })
            .await;
        if let Err(e) = behavior {
            let _ = std::fs::remove_dir_all(&download_dir);
            return Err(e);
        }
        let inner = Arc::new(ContextInner {
            browser,
            id,
            options,
            selectors: host.selectors(),
            hooks: host.hooks(),
            plugin_host: host,
            init_scripts: Mutex::new(Vec::new()),
            routes: Mutex::new(Vec::new()),
            pages: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            creating: tokio::sync::Mutex::new(()),
            claims: Mutex::new(HashMap::new()),
            page_events: broadcast::channel(64).0,
            page_handlers: Mutex::new(Vec::new()),
            download_dir,
            downloads: Mutex::new(HashMap::new()),
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

    /// The options the context was created with.
    pub fn options(&self) -> &ContextOptions {
        &self.inner.options
    }

    /// Opens a new page in this context.
    pub async fn new_page(&self) -> Result<Page> {
        let browser_session = self.inner.browser.browser_session();
        let (tx, rx) = oneshot::channel();
        let target_id = {
            let _gate = self.inner.creating.lock().await;
            let created = browser_session
                .send(&target::CreateTargetParams {
                    url: "about:blank".into(),
                    browser_context_id: Some(self.inner.id.clone()),
                    // Each page gets its own window, so opening a page never hides another: a
                    // background tab does not render, and input sent to it stalls.
                    new_window: Some(true),
                    ..Default::default()
                })
                .await?;
            self.inner
                .claims
                .lock()
                .unwrap()
                .insert(created.target_id.clone(), tx);
            created.target_id
        };
        // The browser attaches the new target (paused) on its own; `adopt_target` sets the
        // page up and hands it over here.
        match tokio::time::timeout(ATTACH_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(Error::target_closed(
                "page",
                Some(&target_id),
                "The new page closed before it was ready.",
            )),
            Err(_) => {
                self.inner.claims.lock().unwrap().remove(&target_id);
                let _ = browser_session
                    .send(&target::CloseTargetParams { target_id })
                    .await;
                Err(Error::other(format!(
                    "The new page was not attached within {}s.",
                    ATTACH_TIMEOUT.as_secs()
                )))
            }
        }
    }

    /// Sets up a page target the browser just attached (paused): one `new_page` created, or
    /// one a page opened itself (a popup), and lets it run.
    pub(crate) async fn adopt_target(
        &self,
        session_id: String,
        info: target::TargetInfo,
        paused: bool,
    ) {
        let claim = {
            let _gate = self.inner.creating.lock().await;
            self.inner.claims.lock().unwrap().remove(&info.target_id)
        };
        let registry = self.inner.browser.registry().clone();
        let session = registry.create_session(&session_id);
        let opener = info
            .opener_id
            .as_deref()
            .and_then(|id| self.page_by_target(id));
        let setup = async {
            Page::initialize(
                self.clone(),
                session.clone(),
                info.target_id.clone(),
                opener.as_ref(),
                &info.url,
                paused,
            )
            .await
        };
        let page = match setup.await {
            Ok(page) => page,
            Err(e) => {
                let _ = session
                    .send(&runtime::RunIfWaitingForDebuggerParams {})
                    .await;
                registry.remove_session(&session_id);
                match claim {
                    Some(tx) => {
                        let _ = self
                            .inner
                            .browser
                            .browser_session()
                            .send(&target::CloseTargetParams {
                                target_id: info.target_id.clone(),
                            })
                            .await;
                        let _ = tx.send(Err(e));
                    }
                    None if !e.is_browser_lost() => {
                        tracing::warn!(target = %info.target_id, error = %e, "a new page could not be set up");
                    }
                    None => {}
                }
                return;
            }
        };
        self.inner.browser.register_page(&session_id, &page.inner);
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
        let handlers = self.inner.page_handlers.lock().unwrap().clone();
        for handler in handlers {
            handler(&page);
        }
        let _ = self.inner.page_events.send(page.clone());
        if let Some(opener) = &opener {
            opener.inner.notify_popup(&page);
        }
        if let Some(tx) = claim {
            let _ = tx.send(Ok(page));
        }
    }

    fn page_by_target(&self, target_id: &str) -> Option<Page> {
        self.pages()
            .into_iter()
            .find(|p| p.target_id() == target_id)
    }

    /// Calls `handler` for every page opened in this context, by `new_page` or by a page
    /// (popups and `target=_blank` links).
    pub fn on_page(&self, handler: impl Fn(&Page) + Send + Sync + 'static) {
        self.inner
            .page_handlers
            .lock()
            .unwrap()
            .push(Arc::new(handler));
    }

    /// Waits for the next page opened in this context. Starts listening as soon as it is
    /// called, so create it before the action that opens the page.
    pub fn wait_for_page(
        &self,
        timeout: Option<Duration>,
    ) -> impl std::future::Future<Output = Result<Page>> + Send + 'static {
        let mut events = self.inner.page_events.subscribe();
        let timeout = timeout.unwrap_or(self.inner.options.navigation_timeout);
        async move {
            let wait = async {
                loop {
                    match events.recv().await {
                        Ok(page) => return Ok(page),
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err(Error::target_closed(
                                "context",
                                None,
                                "The context closed.",
                            ));
                        }
                    }
                }
            };
            tokio::time::timeout(timeout, wait)
                .await
                .unwrap_or_else(|_| {
                    Err(ErrorKind::WaitTimeout {
                        condition: "new page".into(),
                        timeout,
                        last_evaluated_value: None,
                        message: format!("No new page opened within {}ms.", timeout.as_millis()),
                    }
                    .into())
                })
        }
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

    /// Intercepts matching requests from every page in this context, current and future.
    /// Page routes take precedence. See [`Page::route`].
    pub async fn route<F, Fut>(
        &self,
        pattern: impl Into<crate::network::RoutePattern>,
        handler: F,
    ) -> Result<crate::network::RouteId>
    where
        F: Fn(std::sync::Arc<crate::network::Route>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let entry = crate::network::make_route_entry(pattern.into(), handler);
        let id = crate::network::RouteId(entry.id);
        self.inner.routes.lock().unwrap().push(entry);
        for page in self.pages() {
            page.sync_fetch().await?;
        }
        Ok(id)
    }

    /// Removes a route added with [`BrowserContext::route`].
    pub async fn unroute(&self, id: crate::network::RouteId) -> Result<()> {
        self.inner.routes.lock().unwrap().retain(|r| r.id != id.0);
        for page in self.pages() {
            page.sync_fetch().await?;
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

    /// The context's cookies, and the local storage of the origin each of its pages is on. Save
    /// it with [`StorageState::save`](crate::storage::StorageState::save) to start other
    /// contexts from it.
    ///
    /// A cross-site iframe's storage is left out: Chrome partitions it by the site that embeds
    /// it, so it is not the storage that origin has when visited directly, and cannot be set
    /// again the way a page's own storage can. A same-origin iframe shares its page's storage.
    pub async fn storage_state(&self) -> Result<crate::storage::StorageState> {
        use crate::storage::{OriginStorage, StorageItem, StorageState, StoredCookie};
        let result = self
            .inner
            .browser
            .browser_session()
            .send(&storage::GetCookiesParams {
                browser_context_id: Some(self.inner.id.clone()),
            })
            .await?;
        let cookies = result
            .cookies
            .into_iter()
            .map(|c| StoredCookie {
                expires: if c.session { -1.0 } else { c.expires },
                same_site: c.same_site.and_then(same_site_from_cdp),
                name: c.name,
                value: c.value,
                domain: c.domain,
                path: c.path,
                http_only: c.http_only,
                secure: c.secure,
            })
            .collect();

        let mut origins: Vec<OriginStorage> = Vec::new();
        for page in self.pages() {
            // A page on an opaque origin (about:blank, data:) has no storage to keep, and one
            // whose storage the browser refuses to open is skipped.
            let Ok(Some((origin, entries))) = page
                .evaluate::<Option<(String, Vec<(String, String)>)>>(READ_LOCAL_STORAGE)
                .await
            else {
                continue;
            };
            if entries.is_empty() || origins.iter().any(|o| o.origin == origin) {
                continue;
            }
            origins.push(OriginStorage {
                origin,
                local_storage: entries
                    .into_iter()
                    .map(|(name, value)| StorageItem { name, value })
                    .collect(),
            });
        }
        Ok(StorageState { cookies, origins })
    }

    /// Adds the cookies and local storage in `state` to this context.
    ///
    /// Local storage can only be written by a page on its origin, so this opens one temporary
    /// page, takes it to each origin with a route that answers with an empty document (nothing
    /// goes to the network), writes the entries and closes it. Lifecycle hooks see that page.
    pub async fn set_storage_state(&self, state: &crate::storage::StorageState) -> Result<()> {
        if !state.cookies.is_empty() {
            let cookies = state
                .cookies
                .iter()
                .map(|c| network::CookieParam {
                    name: c.name.clone(),
                    value: c.value.clone(),
                    domain: Some(c.domain.clone()),
                    path: Some(c.path.clone()),
                    secure: Some(c.secure),
                    http_only: Some(c.http_only),
                    same_site: c.same_site.map(same_site_to_cdp),
                    expires: (c.expires >= 0.0).then_some(c.expires),
                    ..Default::default()
                })
                .collect();
            self.inner
                .browser
                .browser_session()
                .send(&storage::SetCookiesParams {
                    cookies,
                    browser_context_id: Some(self.inner.id.clone()),
                })
                .await?;
        }

        let origins: Vec<_> = state
            .origins
            .iter()
            .filter(|o| !o.local_storage.is_empty())
            .collect();
        if origins.is_empty() {
            return Ok(());
        }
        let page = self.new_page().await?;
        let written = async {
            page.route("**", |route| async move {
                let _ = route
                    .fulfill(crate::network::FulfillOptions::html(""))
                    .await;
            })
            .await?;
            for origin in origins {
                page.goto(&format!("{}/", origin.origin.trim_end_matches('/')))
                    .await?;
                let entries: Vec<(&str, &str)> = origin
                    .local_storage
                    .iter()
                    .map(|i| (i.name.as_str(), i.value.as_str()))
                    .collect();
                page.evaluate_with::<(), _>(WRITE_LOCAL_STORAGE, &entries)
                    .await?;
            }
            Ok::<(), Error>(())
        }
        .await;
        let _ = page.close().await;
        written
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
        let _ = std::fs::remove_dir_all(&self.inner.download_dir);
        result
    }

    /// Whether the context has been closed.
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

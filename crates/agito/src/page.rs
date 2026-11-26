//! Pages: navigation, evaluation, screenshots, events and the entry points for locators.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use agito_protocol::cdp::{emulation, network, page as cdp_page, runtime, security, target};
use base64::Engine;
use regex::Regex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::context::{BrowserContext, Viewport};
use crate::error::{Error, ErrorKind, Result};
use crate::handles::{decode_value, evaluate_expression, to_arg};
use crate::input::{Keyboard, Mouse};
use crate::locator::Locator;
use crate::transport::Session;

/// When a navigation counts as finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitUntil {
    /// The `load` event fired.
    #[default]
    Load,
    /// The `DOMContentLoaded` event fired.
    DomContentLoaded,
    /// No network connections for at least 500ms.
    NetworkIdle,
    /// Do not wait: return once the navigation is committed.
    Commit,
}

impl WaitUntil {
    fn lifecycle_name(self) -> Option<&'static str> {
        match self {
            WaitUntil::Load => Some("load"),
            WaitUntil::DomContentLoaded => Some("DOMContentLoaded"),
            WaitUntil::NetworkIdle => Some("networkIdle"),
            WaitUntil::Commit => None,
        }
    }
}

/// Options for `goto`, `reload`, `go_back` and `go_forward`.
#[derive(Debug, Clone, Default)]
pub struct NavigateOptions {
    pub wait_until: WaitUntil,
    /// Defaults to the context's navigation timeout.
    pub timeout: Option<Duration>,
    pub referer: Option<String>,
}

/// The main document's HTTP response.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub headers: HashMap<String, String>,
    pub mime_type: String,
}

impl Response {
    /// Whether the status is 2xx.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    fn from_cdp(r: network::Response) -> Self {
        let headers = r
            .headers
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| {
                        (
                            k.to_ascii_lowercase(),
                            v.as_str().unwrap_or_default().to_owned(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            url: r.url,
            status: r.status as u16,
            status_text: r.status_text,
            headers,
            mime_type: r.mime_type,
        }
    }
}

/// Image format for screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Png,
    Jpeg,
    Webp,
}

/// Options for [`Page::screenshot`].
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
    pub format: ImageFormat,
    /// 0-100, for JPEG and WebP.
    pub quality: Option<u8>,
    /// Capture the whole scrollable page, not only the viewport.
    pub full_page: bool,
    /// Capture only this region (CSS pixels, page coordinates).
    pub clip: Option<crate::handles::BoundingBox>,
}

/// Options for [`Page::pdf`]. Sizes are in inches.
#[derive(Debug, Clone, Default)]
pub struct PdfOptions {
    pub landscape: bool,
    pub print_background: bool,
    pub scale: Option<f64>,
    pub paper_width: Option<f64>,
    pub paper_height: Option<f64>,
    pub page_ranges: Option<String>,
}

/// A message the page logged to its console.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleMessage {
    /// `log`, `error`, `warning`, `info`, `debug`, ...
    pub kind: String,
    pub text: String,
    pub args: Vec<Value>,
}

/// What a page's URL is matched against: an exact URL, a glob (`*` within a path segment,
/// `**` across them), a regex, or a predicate.
#[derive(Clone)]
pub enum UrlMatcher {
    Glob(String),
    Regex(Regex),
    Predicate(Arc<dyn Fn(&str) -> bool + Send + Sync>),
}

impl UrlMatcher {
    pub fn matches(&self, url: &str) -> bool {
        match self {
            UrlMatcher::Glob(pattern) => glob_matches(pattern, url),
            UrlMatcher::Regex(re) => re.is_match(url),
            UrlMatcher::Predicate(f) => f(url),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            UrlMatcher::Glob(p) => format!("'{p}'"),
            UrlMatcher::Regex(r) => format!("/{r}/"),
            UrlMatcher::Predicate(_) => "a predicate".to_owned(),
        }
    }
}

impl From<&str> for UrlMatcher {
    fn from(s: &str) -> Self {
        UrlMatcher::Glob(s.to_owned())
    }
}

impl From<String> for UrlMatcher {
    fn from(s: String) -> Self {
        UrlMatcher::Glob(s)
    }
}

impl From<Regex> for UrlMatcher {
    fn from(r: Regex) -> Self {
        UrlMatcher::Regex(r)
    }
}

/// Glob match on a whole URL: `**` matches anything, `*` anything but `/`. A pattern with no
/// `*` must equal the URL.
pub fn glob_matches(pattern: &str, url: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == url;
    }
    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                re.push_str(".*");
            } else {
                re.push_str("[^/]*");
            }
        } else {
            re.push_str(&regex::escape(&c.to_string()));
        }
    }
    re.push('$');
    Regex::new(&re).is_ok_and(|r| r.is_match(url))
}

/// A JavaScript dialog (`alert`, `confirm`, `prompt`, `beforeunload`).
#[derive(Clone)]
pub struct Dialog {
    session: Arc<dyn Session>,
    pub kind: String,
    pub message: String,
    pub default_prompt: Option<String>,
}

impl std::fmt::Debug for Dialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dialog")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .finish()
    }
}

impl Dialog {
    /// Accepts the dialog, answering a prompt with `prompt_text`.
    pub async fn accept(&self, prompt_text: Option<&str>) -> Result<()> {
        self.session
            .send(&cdp_page::HandleJavaScriptDialogParams {
                accept: true,
                prompt_text: prompt_text.map(str::to_owned),
            })
            .await?;
        Ok(())
    }

    /// Dismisses the dialog.
    pub async fn dismiss(&self) -> Result<()> {
        self.session
            .send(&cdp_page::HandleJavaScriptDialogParams {
                accept: false,
                prompt_text: None,
            })
            .await?;
        Ok(())
    }
}

type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type DialogHandler = Arc<dyn Fn(Dialog) -> BoxFuture + Send + Sync>;
type ConsoleHandler = Arc<dyn Fn(&ConsoleMessage) + Send + Sync>;
type ErrorHandler = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone)]
pub(crate) enum PageEvent {
    Lifecycle {
        loader_id: String,
        name: String,
    },
    /// The main frame committed a document (`loader_id`), or moved within one (`None`).
    Navigated {
        url: String,
        loader_id: Option<String>,
    },
    Closed,
}

#[derive(Default)]
struct PageState {
    main_frame_id: String,
    url: String,
    /// The main frame's current document.
    loader_id: Option<String>,
    /// Lifecycle events seen per document, newest documents last.
    lifecycle: Vec<(String, HashSet<String>)>,
    document_responses: HashMap<String, Response>,
}

impl PageState {
    fn reached(&self, loader_id: &str, name: &str) -> bool {
        self.lifecycle
            .iter()
            .any(|(l, names)| l == loader_id && names.contains(name))
    }

    fn record(&mut self, loader_id: &str, name: &str) {
        match self.lifecycle.iter_mut().find(|(l, _)| l == loader_id) {
            Some((_, names)) => {
                names.insert(name.to_owned());
            }
            None => {
                if self.lifecycle.len() == 16 {
                    self.lifecycle.remove(0);
                }
                self.lifecycle
                    .push((loader_id.to_owned(), HashSet::from([name.to_owned()])));
            }
        }
    }
}

/// A browser tab. Cheap to clone.
#[derive(Clone)]
pub struct Page {
    pub(crate) inner: Arc<PageInner>,
}

pub(crate) struct PageInner {
    pub context: BrowserContext,
    pub session: Arc<dyn Session>,
    target_id: String,
    state: Mutex<PageState>,
    events: broadcast::Sender<PageEvent>,
    console_handlers: Mutex<Vec<ConsoleHandler>>,
    error_handlers: Mutex<Vec<ErrorHandler>>,
    dialog_handlers: Mutex<Vec<DialogHandler>>,
    mouse: Mouse,
    keyboard: Keyboard,
    closed: AtomicBool,
    pump: Mutex<Option<JoinHandle<()>>>,
}

impl PageInner {
    pub(crate) fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.events.send(PageEvent::Closed);
        self.session.cleanup_subscriptions();
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl Drop for PageInner {
    fn drop(&mut self) {
        if let Some(pump) = self.pump.get_mut().unwrap().take() {
            pump.abort();
        }
    }
}

const PAGE_EVENTS: &[&str] = &[
    "Page.lifecycleEvent",
    "Page.frameNavigated",
    "Page.navigatedWithinDocument",
    "Page.javascriptDialogOpening",
    "Runtime.consoleAPICalled",
    "Runtime.exceptionThrown",
    "Network.responseReceived",
    "Inspector.targetCrashed",
];

impl Page {
    pub(crate) fn from_inner(inner: Arc<PageInner>) -> Self {
        Self { inner }
    }

    pub(crate) async fn initialize(
        context: BrowserContext,
        session: Arc<dyn Session>,
        target_id: String,
    ) -> Result<Page> {
        let (events, _) = broadcast::channel(1024);
        let inner = Arc::new(PageInner {
            context,
            session: session.clone(),
            target_id,
            state: Mutex::new(PageState::default()),
            events,
            console_handlers: Mutex::new(Vec::new()),
            error_handlers: Mutex::new(Vec::new()),
            dialog_handlers: Mutex::new(Vec::new()),
            mouse: Mouse::new(session.clone()),
            keyboard: Keyboard::new(session.clone()),
            closed: AtomicBool::new(false),
            pump: Mutex::new(None),
        });
        // Subscribe before enabling domains so nothing they announce is missed.
        let stream = session.subscribe(PAGE_EVENTS);
        let pump = tokio::spawn(event_pump(Arc::downgrade(&inner), stream));
        *inner.pump.lock().unwrap() = Some(pump);
        let page = Page { inner };

        session.send(&cdp_page::EnableParams::default()).await?;
        session
            .send(&cdp_page::SetLifecycleEventsEnabledParams { enabled: true })
            .await?;
        session.send(&runtime::EnableParams::default()).await?;
        session.send(&network::EnableParams::default()).await?;

        let options = page.inner.context.options().clone();
        if let Some(viewport) = options.viewport {
            page.set_viewport(viewport).await?;
        }
        if options.user_agent.is_some() || options.locale.is_some() {
            let user_agent = match &options.user_agent {
                Some(ua) => ua.clone(),
                None => {
                    let version = session
                        .send(&agito_protocol::cdp::browser::GetVersionParams::default())
                        .await?;
                    version.user_agent
                }
            };
            session
                .send(&emulation::SetUserAgentOverrideParams {
                    user_agent,
                    accept_language: options.locale.clone(),
                    ..Default::default()
                })
                .await?;
        }
        if let Some(locale) = &options.locale {
            session
                .send(&emulation::SetLocaleOverrideParams {
                    locale: Some(locale.clone()),
                })
                .await?;
        }
        if let Some(tz) = &options.timezone_id {
            session
                .send(&emulation::SetTimezoneOverrideParams {
                    timezone_id: tz.clone(),
                })
                .await?;
        }
        if let Some(scheme) = options.color_scheme_css() {
            session
                .send(&emulation::SetEmulatedMediaParams {
                    media: None,
                    features: Some(vec![emulation::MediaFeature {
                        name: "prefers-color-scheme".into(),
                        value: scheme.into(),
                    }]),
                })
                .await?;
        }
        if options.ignore_https_errors {
            session
                .send(&security::SetIgnoreCertificateErrorsParams { ignore: true })
                .await?;
        }
        if !options.extra_http_headers.is_empty() {
            session
                .send(&network::SetExtraHTTPHeadersParams {
                    headers: serde_json::to_value(&options.extra_http_headers).unwrap(),
                })
                .await?;
        }
        let scripts = page
            .inner
            .context
            .inner
            .init_scripts
            .lock()
            .unwrap()
            .clone();
        for script in scripts {
            page.add_init_script(script).await?;
        }

        let tree = session
            .send(&cdp_page::GetFrameTreeParams::default())
            .await?;
        {
            let mut state = page.inner.state.lock().unwrap();
            state.main_frame_id = tree.frame_tree.frame.id.clone();
            state.url = tree.frame_tree.frame.url.clone();
            let loader = tree.frame_tree.frame.loader_id.clone();
            if state.loader_id.is_none() {
                state.loader_id = Some(loader.clone());
            }
            // A new tab's initial about:blank finished loading before lifecycle events were on.
            if !state.lifecycle.iter().any(|(l, _)| *l == loader) {
                for name in ["DOMContentLoaded", "load", "networkIdle"] {
                    state.record(&loader, name);
                }
            }
        }
        Ok(page)
    }

    /// The context this page belongs to.
    pub fn context(&self) -> &BrowserContext {
        &self.inner.context
    }

    /// The protocol's id for this page's target.
    pub fn target_id(&self) -> &str {
        &self.inner.target_id
    }

    pub(crate) fn session(&self) -> &Arc<dyn Session> {
        &self.inner.session
    }

    /// The main frame's current URL.
    pub fn url(&self) -> String {
        self.inner.state.lock().unwrap().url.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    pub fn mouse(&self) -> &Mouse {
        &self.inner.mouse
    }

    pub fn keyboard(&self) -> &Keyboard {
        &self.inner.keyboard
    }

    fn ensure_open(&self) -> Result<()> {
        if self.is_closed() {
            return Err(Error::target_closed(
                "page",
                Some(&self.inner.target_id),
                "The page has been closed.",
            ));
        }
        Ok(())
    }

    fn resolve_url(&self, url: &str) -> String {
        let Some(base) = &self.inner.context.options().base_url else {
            return url.to_owned();
        };
        if url.contains("://")
            || url.starts_with("data:")
            || url.starts_with("about:")
            || url.starts_with("javascript:")
        {
            return url.to_owned();
        }
        let base = base.trim_end_matches('/');
        if url.starts_with('/') {
            format!("{base}{url}")
        } else {
            format!("{base}/{url}")
        }
    }

    fn navigation_timeout(&self, options: &NavigateOptions) -> Duration {
        options
            .timeout
            .unwrap_or(self.inner.context.options().navigation_timeout)
    }

    /// Navigates to `url` and waits for the `load` event. Returns the main document's response,
    /// or `None` for navigations without one (same-document, `about:blank`, `data:` URLs).
    pub async fn goto(&self, url: &str) -> Result<Option<Response>> {
        self.goto_with(url, NavigateOptions::default()).await
    }

    /// [`Page::goto`] with options.
    pub async fn goto_with(&self, url: &str, options: NavigateOptions) -> Result<Option<Response>> {
        self.ensure_open()?;
        let url = self.resolve_url(url);
        self.fire_hooks_before_navigation(&url).await;
        let timeout = self.navigation_timeout(&options);
        let mut events = self.inner.events.subscribe();
        let result = self
            .inner
            .session
            .send(&cdp_page::NavigateParams {
                url: url.clone(),
                referrer: options.referer.clone(),
                ..Default::default()
            })
            .await?;
        if let Some(error) = result.error_text.filter(|e| !e.is_empty()) {
            return Err(ErrorKind::Navigation {
                url: url.clone(),
                error_code: error.clone(),
                page_url: self.url(),
                message: format!("Navigation to {url} failed: {error}"),
            }
            .into());
        }
        let response = match result.loader_id {
            // Same-document navigation: nothing more will load.
            None => None,
            Some(loader_id) => {
                if let Some(name) = options.wait_until.lifecycle_name() {
                    self.wait_for_lifecycle(
                        &mut events,
                        Some(loader_id.clone()),
                        name,
                        &url,
                        timeout,
                    )
                    .await?;
                }
                self.inner
                    .state
                    .lock()
                    .unwrap()
                    .document_responses
                    .remove(&loader_id)
            }
        };
        self.fire_hooks_after_navigation(response.as_ref()).await;
        Ok(response)
    }

    /// Reloads the page.
    pub async fn reload(&self) -> Result<Option<Response>> {
        self.ensure_open()?;
        let url = self.url();
        let mut events = self.inner.events.subscribe();
        self.inner
            .session
            .send(&cdp_page::ReloadParams::default())
            .await?;
        self.finish_history_navigation(&mut events, &url, NavigateOptions::default())
            .await
    }

    /// Goes back in history; `None` when there is nothing to go back to.
    pub async fn go_back(&self) -> Result<Option<Response>> {
        self.go_history(-1).await
    }

    /// Goes forward in history; `None` when there is nothing to go forward to.
    pub async fn go_forward(&self) -> Result<Option<Response>> {
        self.go_history(1).await
    }

    async fn go_history(&self, delta: i64) -> Result<Option<Response>> {
        self.ensure_open()?;
        let history = self
            .inner
            .session
            .send(&cdp_page::GetNavigationHistoryParams::default())
            .await?;
        let index = history.current_index + delta;
        if index < 0 || index >= history.entries.len() as i64 {
            return Ok(None);
        }
        let entry = &history.entries[index as usize];
        let mut events = self.inner.events.subscribe();
        self.inner
            .session
            .send(&cdp_page::NavigateToHistoryEntryParams { entry_id: entry.id })
            .await?;
        self.finish_history_navigation(&mut events, &entry.url.clone(), NavigateOptions::default())
            .await
    }

    /// Waits for a navigation started without knowing its document: the main frame's next
    /// commit names it, then its lifecycle event finishes it. A page restored from the
    /// back-forward cache commits a document that has already loaded.
    async fn finish_history_navigation(
        &self,
        events: &mut broadcast::Receiver<PageEvent>,
        url: &str,
        options: NavigateOptions,
    ) -> Result<Option<Response>> {
        let timeout = self.navigation_timeout(&options);
        let Some(name) = options.wait_until.lifecycle_name() else {
            return Ok(None);
        };
        let deadline = Instant::now() + timeout;
        let loader_id = loop {
            match self.next_event(events, deadline).await {
                Some(PageEvent::Navigated {
                    loader_id: Some(l), ..
                }) => break l,
                // A navigation within the document (a hash change) loads nothing more.
                Some(PageEvent::Navigated {
                    loader_id: None, ..
                }) => return Ok(None),
                Some(PageEvent::Closed) => return Err(self.closed_error()),
                Some(_) => continue,
                None => return Err(navigation_timeout(url, timeout)),
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        self.wait_for_lifecycle(events, Some(loader_id.clone()), name, url, remaining)
            .await?;
        Ok(self
            .inner
            .state
            .lock()
            .unwrap()
            .document_responses
            .remove(&loader_id))
    }

    async fn next_event(
        &self,
        events: &mut broadcast::Receiver<PageEvent>,
        deadline: Instant,
    ) -> Option<PageEvent> {
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match tokio::time::timeout(remaining, events.recv()).await {
                Ok(Ok(event)) => return Some(event),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => return Some(PageEvent::Closed),
                Err(_) => return None,
            }
        }
    }

    async fn wait_for_lifecycle(
        &self,
        events: &mut broadcast::Receiver<PageEvent>,
        loader_id: Option<String>,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<()> {
        {
            let state = self.inner.state.lock().unwrap();
            let wanted = loader_id.as_ref().or(state.loader_id.as_ref());
            if wanted.is_some_and(|l| state.reached(l, name)) {
                return Ok(());
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            match self.next_event(events, deadline).await {
                Some(PageEvent::Lifecycle {
                    loader_id: l,
                    name: n,
                }) => {
                    let current = self.inner.state.lock().unwrap().loader_id.clone();
                    if n == name
                        && loader_id
                            .as_ref()
                            .or(current.as_ref())
                            .is_none_or(|want| *want == l)
                    {
                        return Ok(());
                    }
                }
                Some(PageEvent::Closed) => return Err(self.closed_error()),
                Some(PageEvent::Navigated { .. }) => {}
                None => return Err(navigation_timeout(url, timeout)),
            }
        }
    }

    fn closed_error(&self) -> Error {
        Error::target_closed(
            "page",
            Some(&self.inner.target_id),
            "The page closed while waiting for it.",
        )
    }

    /// Waits until the current document has reached `state` (`load` by default).
    pub async fn wait_for_load_state(
        &self,
        state: WaitUntil,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.ensure_open()?;
        let Some(name) = state.lifecycle_name() else {
            return Ok(());
        };
        let mut events = self.inner.events.subscribe();
        let url = self.url();
        let timeout = timeout.unwrap_or(self.inner.context.options().navigation_timeout);
        self.wait_for_lifecycle(&mut events, None, name, &url, timeout)
            .await
    }

    /// Waits until the main frame's URL matches.
    pub async fn wait_for_url(
        &self,
        matcher: impl Into<UrlMatcher>,
        timeout: Option<Duration>,
    ) -> Result<()> {
        let matcher = matcher.into();
        let timeout = timeout.unwrap_or(self.inner.context.options().navigation_timeout);
        let mut events = self.inner.events.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            if matcher.matches(&self.url()) {
                return Ok(());
            }
            let poll = Instant::now() + Duration::from_millis(100);
            match self.next_event(&mut events, poll.min(deadline)).await {
                Some(PageEvent::Navigated { url, .. }) if matcher.matches(&url) => return Ok(()),
                Some(PageEvent::Closed) => return Err(self.closed_error()),
                _ => {}
            }
            if Instant::now() >= deadline {
                let last = self.url();
                return Err(ErrorKind::WaitTimeout {
                    condition: format!("URL match {}", matcher.describe()),
                    timeout,
                    last_evaluated_value: Some(last.clone()),
                    message: format!(
                        "wait_for_url timed out waiting for {} after {}ms. The URL was '{last}'.",
                        matcher.describe(),
                        timeout.as_millis()
                    ),
                }
                .into());
            }
        }
    }

    /// Sleeps. Prefer waiting for a condition.
    pub async fn wait_for_timeout(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    /// Evaluates a JavaScript expression in the page and decodes its value. A function
    /// expression (`() => document.title`) is called; a promise is awaited.
    pub async fn evaluate<T: DeserializeOwned>(&self, expression: &str) -> Result<T> {
        self.ensure_open()?;
        let wrapped = format!(
            "(async () => {{ const __v = ({expression}); return typeof __v === 'function' ? await __v() : await __v; }})()"
        );
        let result = evaluate_expression(&self.inner.session, &wrapped, true).await?;
        decode_value(result)
    }

    /// Calls a JavaScript function with one serializable argument.
    pub async fn evaluate_with<T: DeserializeOwned, A: Serialize>(
        &self,
        function: &str,
        arg: &A,
    ) -> Result<T> {
        self.ensure_open()?;
        let arg = to_arg(arg)?;
        let expression = format!("(async () => await ({function})({arg}))()");
        let result = evaluate_expression(&self.inner.session, &expression, true).await?;
        decode_value(result)
    }

    /// The document's title.
    pub async fn title(&self) -> Result<String> {
        self.evaluate("document.title").await
    }

    /// The full HTML of the document, doctype included.
    pub async fn content(&self) -> Result<String> {
        self.evaluate(
            "(() => { let s = ''; if (document.doctype) s = new XMLSerializer().serializeToString(document.doctype); \
             if (document.documentElement) s += document.documentElement.outerHTML; return s; })",
        )
        .await
    }

    /// Replaces the document with `html`.
    pub async fn set_content(&self, html: &str) -> Result<()> {
        self.ensure_open()?;
        let frame_id = self.inner.state.lock().unwrap().main_frame_id.clone();
        self.inner
            .session
            .send(&cdp_page::SetDocumentContentParams {
                frame_id,
                html: html.to_owned(),
            })
            .await?;
        Ok(())
    }

    /// Adds a script that runs before the page's own scripts on every navigation.
    pub async fn add_init_script(&self, script: impl Into<String>) -> Result<()> {
        self.inner
            .session
            .send(&cdp_page::AddScriptToEvaluateOnNewDocumentParams {
                source: script.into(),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Resizes the viewport.
    pub async fn set_viewport(&self, viewport: Viewport) -> Result<()> {
        self.inner
            .session
            .send(&emulation::SetDeviceMetricsOverrideParams {
                width: viewport.width.into(),
                height: viewport.height.into(),
                device_scale_factor: 1.0,
                mobile: false,
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Captures a screenshot and returns the encoded image.
    pub async fn screenshot(&self, options: ScreenshotOptions) -> Result<Vec<u8>> {
        self.ensure_open()?;
        let format = match options.format {
            ImageFormat::Png => cdp_page::CaptureScreenshotParamsFormat::Png,
            ImageFormat::Jpeg => cdp_page::CaptureScreenshotParamsFormat::Jpeg,
            ImageFormat::Webp => cdp_page::CaptureScreenshotParamsFormat::Webp,
        };
        let clip = if let Some(b) = options.clip {
            Some(cdp_page::Viewport {
                x: b.x,
                y: b.y,
                width: b.width,
                height: b.height,
                scale: 1.0,
            })
        } else if options.full_page {
            let metrics = self
                .inner
                .session
                .send(&cdp_page::GetLayoutMetricsParams::default())
                .await?;
            let size = metrics.css_content_size;
            Some(cdp_page::Viewport {
                x: 0.0,
                y: 0.0,
                width: size.width,
                height: size.height,
                scale: 1.0,
            })
        } else {
            None
        };
        let result = self
            .inner
            .session
            .send(&cdp_page::CaptureScreenshotParams {
                format: Some(format),
                quality: options.quality.map(i64::from),
                clip,
                capture_beyond_viewport: Some(options.full_page),
                ..Default::default()
            })
            .await?;
        decode_base64(&result.data)
    }

    /// Prints the page to PDF (headless only).
    pub async fn pdf(&self, options: PdfOptions) -> Result<Vec<u8>> {
        self.ensure_open()?;
        let result = self
            .inner
            .session
            .send(&cdp_page::PrintToPDFParams {
                landscape: Some(options.landscape),
                print_background: Some(options.print_background),
                scale: options.scale,
                paper_width: options.paper_width,
                paper_height: options.paper_height,
                page_ranges: options.page_ranges,
                ..Default::default()
            })
            .await?;
        decode_base64(&result.data)
    }

    /// Calls `handler` for every console message.
    pub fn on_console(&self, handler: impl Fn(&ConsoleMessage) + Send + Sync + 'static) {
        self.inner
            .console_handlers
            .lock()
            .unwrap()
            .push(Arc::new(handler));
    }

    /// Calls `handler` with the message of every uncaught exception in the page.
    pub fn on_page_error(&self, handler: impl Fn(&str) + Send + Sync + 'static) {
        self.inner
            .error_handlers
            .lock()
            .unwrap()
            .push(Arc::new(handler));
    }

    /// Handles JavaScript dialogs. Without a handler, dialogs are dismissed automatically.
    /// The handler must accept or dismiss the dialog, or the page stays blocked.
    pub fn on_dialog<F, Fut>(&self, handler: F)
    where
        F: Fn(Dialog) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.inner
            .dialog_handlers
            .lock()
            .unwrap()
            .push(Arc::new(move |d| Box::pin(handler(d))));
    }

    /// Closes the page.
    pub async fn close(&self) -> Result<()> {
        if self.is_closed() {
            return Ok(());
        }
        let browser = self.inner.context.browser().clone();
        let result = browser
            .browser_session()
            .send(&target::CloseTargetParams {
                target_id: self.inner.target_id.clone(),
            })
            .await;
        self.inner.mark_closed();
        for hook in &self.inner.context.inner.hooks {
            if let Err(e) = hook.on_page_closed(self).await {
                tracing::warn!(error = %e, "lifecycle hook on_page_closed failed");
            }
        }
        match result {
            Err(e) if !e.is_browser_lost() => Err(e),
            _ => Ok(()),
        }
    }

    /// A locator for `selector`. Selectors are CSS by default; a `prefix=` picks another
    /// strategy (`text=`, `xpath=`, `role=`, `data-testid=`, ...).
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(self.clone(), selector)
    }

    /// Elements with an ARIA role and, optionally, accessible name.
    pub fn get_by_role(&self, role: &str, name: Option<&str>) -> Locator {
        self.locator(&crate::selectors::role_selector(role, name))
    }

    /// Elements whose text contains `text` (case-insensitive). See [`Page::get_by_text_exact`].
    pub fn get_by_text(&self, text: &str) -> Locator {
        self.locator(&crate::selectors::get_by_text(text, false))
    }

    /// Elements whose whole text equals `text`.
    pub fn get_by_text_exact(&self, text: &str) -> Locator {
        self.locator(&crate::selectors::get_by_text(text, true))
    }

    /// Form controls by their label text.
    pub fn get_by_label(&self, text: &str) -> Locator {
        self.locator(&crate::selectors::get_by_label(text, false))
    }

    /// Inputs by placeholder text.
    pub fn get_by_placeholder(&self, text: &str) -> Locator {
        self.locator(&crate::selectors::get_by_attribute(
            "placeholder",
            text,
            false,
        ))
    }

    /// Elements by `title` attribute.
    pub fn get_by_title(&self, text: &str) -> Locator {
        self.locator(&crate::selectors::get_by_attribute("title", text, false))
    }

    /// Images by alt text.
    pub fn get_by_alt_text(&self, text: &str) -> Locator {
        self.locator(&crate::selectors::get_by_attribute("alt", text, false))
    }

    /// Elements by `data-testid`.
    pub fn get_by_test_id(&self, test_id: &str) -> Locator {
        self.locator(&crate::selectors::test_id_selector(test_id))
    }

    async fn fire_hooks_before_navigation(&self, url: &str) {
        for hook in &self.inner.context.inner.hooks {
            if let Err(e) = hook.before_navigation(self, url).await {
                tracing::warn!(error = %e, "lifecycle hook before_navigation failed");
            }
        }
    }

    async fn fire_hooks_after_navigation(&self, response: Option<&Response>) {
        for hook in &self.inner.context.inner.hooks {
            if let Err(e) = hook.after_navigation(self, response).await {
                tracing::warn!(error = %e, "lifecycle hook after_navigation failed");
            }
        }
    }
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("target_id", &self.inner.target_id)
            .field("url", &self.url())
            .finish()
    }
}

fn decode_base64(data: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| Error::other(format!("the browser sent invalid base64: {e}")))
}

fn navigation_timeout(url: &str, timeout: Duration) -> Error {
    ErrorKind::NavigationTimeout {
        url: url.to_owned(),
        timeout,
        last_network_events: Vec::new(),
        message: format!(
            "Navigation to {url} timed out after {}ms.",
            timeout.as_millis()
        ),
    }
    .into()
}

fn console_text(args: &[runtime::RemoteObject]) -> String {
    args.iter()
        .map(|a| match &a.value {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => a
                .description
                .clone()
                .or_else(|| a.unserializable_value.clone())
                .unwrap_or_else(|| "undefined".into()),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn event_pump(page: Weak<PageInner>, mut stream: crate::transport::EventStream) {
    while let Some(event) = stream.next().await {
        let Some(page) = page.upgrade() else { return };
        let params = event.params;
        match event.method.as_str() {
            "Page.lifecycleEvent" => {
                let Ok(e) = serde_json::from_value::<cdp_page::LifecycleEventEvent>(params) else {
                    continue;
                };
                let mut state = page.state.lock().unwrap();
                if !state.main_frame_id.is_empty() && e.frame_id != state.main_frame_id {
                    continue;
                }
                if e.name == "init" {
                    state.loader_id = Some(e.loader_id.clone());
                }
                state.record(&e.loader_id, &e.name);
                drop(state);
                let _ = page.events.send(PageEvent::Lifecycle {
                    loader_id: e.loader_id,
                    name: e.name,
                });
            }
            "Page.frameNavigated" => {
                let Some(frame) = params.get("frame") else {
                    continue;
                };
                if frame.get("parentId").is_some() {
                    continue;
                }
                let url = frame["url"].as_str().unwrap_or_default().to_owned();
                let loader_id = frame["loaderId"].as_str().map(str::to_owned);
                {
                    let mut state = page.state.lock().unwrap();
                    if let Some(id) = frame["id"].as_str() {
                        state.main_frame_id = id.to_owned();
                    }
                    state.url = url.clone();
                    if loader_id.is_some() {
                        state.loader_id = loader_id.clone();
                    }
                }
                let _ = page.events.send(PageEvent::Navigated { url, loader_id });
            }
            "Page.navigatedWithinDocument" => {
                let mut state = page.state.lock().unwrap();
                if params["frameId"].as_str() != Some(state.main_frame_id.as_str()) {
                    continue;
                }
                let url = params["url"].as_str().unwrap_or_default().to_owned();
                state.url = url.clone();
                drop(state);
                let _ = page.events.send(PageEvent::Navigated {
                    url,
                    loader_id: None,
                });
            }
            "Network.responseReceived" => {
                let Ok(e) = serde_json::from_value::<network::ResponseReceivedEvent>(params) else {
                    continue;
                };
                if e.r#type != network::ResourceType::Document {
                    continue;
                }
                let mut state = page.state.lock().unwrap();
                if e.frame_id
                    .as_deref()
                    .is_some_and(|f| f != state.main_frame_id)
                {
                    continue;
                }
                state
                    .document_responses
                    .insert(e.loader_id, Response::from_cdp(e.response));
            }
            "Runtime.consoleAPICalled" => {
                let Ok(e) = serde_json::from_value::<runtime::ConsoleAPICalledEvent>(params) else {
                    continue;
                };
                let message = ConsoleMessage {
                    kind: serde_json::to_value(e.r#type)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    text: console_text(&e.args),
                    args: e
                        .args
                        .iter()
                        .map(|a| a.value.clone().unwrap_or(Value::Null))
                        .collect(),
                };
                let handlers = page.console_handlers.lock().unwrap().clone();
                for handler in handlers {
                    handler(&message);
                }
            }
            "Runtime.exceptionThrown" => {
                let Ok(e) = serde_json::from_value::<runtime::ExceptionThrownEvent>(params) else {
                    continue;
                };
                let details = e.exception_details;
                let text = details
                    .exception
                    .and_then(|x| x.description)
                    .unwrap_or(details.text);
                let handlers = page.error_handlers.lock().unwrap().clone();
                for handler in handlers {
                    handler(&text);
                }
            }
            "Page.javascriptDialogOpening" => {
                let dialog = Dialog {
                    session: page.session.clone(),
                    kind: params["type"].as_str().unwrap_or("alert").to_owned(),
                    message: params["message"].as_str().unwrap_or_default().to_owned(),
                    default_prompt: params["defaultPrompt"].as_str().map(str::to_owned),
                };
                let handlers = page.dialog_handlers.lock().unwrap().clone();
                if handlers.is_empty() {
                    tokio::spawn(async move {
                        let _ = dialog.dismiss().await;
                    });
                } else {
                    for handler in handlers {
                        tokio::spawn(handler(dialog.clone()));
                    }
                }
            }
            "Inspector.targetCrashed" => page.mark_closed(),
            _ => {}
        }
    }
    if let Some(page) = page.upgrade() {
        page.mark_closed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_matches(
            "**/search**",
            "https://example.com/search?q=1"
        ));
        assert!(glob_matches(
            "https://example.com/*",
            "https://example.com/a"
        ));
        assert!(!glob_matches(
            "https://example.com/*",
            "https://example.com/a/b"
        ));
        assert!(glob_matches(
            "https://example.com/a",
            "https://example.com/a"
        ));
        assert!(!glob_matches(
            "https://example.com/a",
            "https://example.com/a?x"
        ));
        assert!(glob_matches("**/a.b?c=*", "http://h/a.b?c=1"));
    }
}

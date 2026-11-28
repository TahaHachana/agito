//! Pages: tabs, their frames, navigation, screenshots, events and input.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use agito_protocol::cdp::{emulation, fetch, network, page as cdp_page, runtime, security, target};
use base64::Engine;
use regex::Regex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::context::{BrowserContext, Viewport};
use crate::error::{Error, ErrorKind, Result};
use crate::files::{Download, FileChooser};
use crate::frame::{Frame, FrameLocator};
use crate::input::{Keyboard, Mouse};
use crate::locator::Locator;
use crate::network::{
    NetworkEvent, NetworkState, Request, RequestStateUpdate, ResponseData, RouteId, RoutePattern,
    find_route, handle_paused, make_route_entry,
};
use crate::transport::{Session, SessionRegistry};

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
    pub(crate) fn lifecycle_name(self) -> Option<&'static str> {
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
    /// When the navigation counts as done.
    pub wait_until: WaitUntil,
    /// Defaults to the context's navigation timeout.
    pub timeout: Option<Duration>,
    /// The `Referer` header to send.
    pub referer: Option<String>,
}

pub use crate::network::Response;

/// Image format for screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    /// PNG, lossless.
    #[default]
    Png,
    /// JPEG.
    Jpeg,
    /// WebP.
    Webp,
}

/// Options for [`Page::screenshot`].
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
    /// The image format.
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
    /// Print in landscape.
    pub landscape: bool,
    /// Print background colours and images.
    pub print_background: bool,
    /// Scale the rendering, from 0.1 to 2.
    pub scale: Option<f64>,
    /// Paper width in inches.
    pub paper_width: Option<f64>,
    /// Paper height in inches.
    pub paper_height: Option<f64>,
    /// Pages to print, such as `1-5, 8`.
    pub page_ranges: Option<String>,
}

/// A message the page logged to its console.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleMessage {
    /// `log`, `error`, `warning`, `info`, `debug`, ...
    pub kind: String,
    /// The message's text.
    pub text: String,
    /// The values passed to the console call.
    pub args: Vec<Value>,
}

/// What a page's URL is matched against: an exact URL, a glob (`*` within a path segment,
/// `**` across them), a regex, or a predicate.
#[derive(Clone)]
pub enum UrlMatcher {
    /// A glob, or an exact URL when it has no `*`.
    Glob(String),
    /// A regular expression.
    Regex(Regex),
    /// A function of the URL.
    Predicate(Arc<dyn Fn(&str) -> bool + Send + Sync>),
}

impl UrlMatcher {
    /// Whether `url` matches.
    pub fn matches(&self, url: &str) -> bool {
        match self {
            UrlMatcher::Glob(pattern) => glob_matches(pattern, url),
            UrlMatcher::Regex(re) => re.is_match(url),
            UrlMatcher::Predicate(f) => f(url),
        }
    }

    /// The pattern, for error messages.
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
    /// `alert`, `confirm`, `prompt` or `beforeunload`.
    pub kind: String,
    /// The text the dialog shows.
    pub message: String,
    /// A prompt's default answer.
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
type PageHandler = Arc<dyn Fn(&Page) + Send + Sync>;
type FileChooserHandler = Arc<dyn Fn(FileChooser) -> BoxFuture + Send + Sync>;
type DownloadHandler = Arc<dyn Fn(&Download) + Send + Sync>;
type RequestHandler = Arc<dyn Fn(&Request) + Send + Sync>;
type ResponseHandler = Arc<dyn Fn(&Response) + Send + Sync>;

/// Handlers for network events, by kind.
#[derive(Default)]
struct NetworkHandlers {
    request: Vec<RequestHandler>,
    response: Vec<ResponseHandler>,
    finished: Vec<RequestHandler>,
    failed: Vec<RequestHandler>,
}

#[derive(Debug, Clone)]
pub(crate) enum PageEvent {
    Lifecycle {
        frame_id: String,
        loader_id: String,
        name: String,
    },
    /// A frame committed a document (`loader_id`), or moved within one (`None`).
    Navigated {
        frame_id: String,
        url: String,
        loader_id: Option<String>,
    },
    /// A frame's default execution context is ready; waiters re-check the context table.
    ContextCreated,
    FrameDetached {
        frame_id: String,
    },
    /// This page opened another one.
    Popup(Page),
    /// A file input asked for files.
    FileChooser(FileChooser),
    /// The page started a download.
    Download(Download),
    Closed,
}

/// What the page knows about one frame.
#[derive(Debug, Clone, Default)]
pub(crate) struct FrameData {
    pub parent_id: Option<String>,
    pub name: String,
    pub url: String,
    pub loader_id: Option<String>,
    /// The session that owns the frame: `None` for the page's own session, or the session of
    /// the out-of-process iframe target it lives in.
    pub session_id: Option<String>,
}

#[derive(Default)]
pub(crate) struct PageState {
    pub main_frame_id: String,
    pub frames: HashMap<String, FrameData>,
    /// Attach order of frames, so listings come out in document-ish order.
    pub frame_order: Vec<String>,
    /// Default-world execution context per frame: (context id, owning session id).
    pub contexts: HashMap<String, (i64, Option<String>)>,
    /// Lifecycle events seen per document, newest documents last.
    lifecycle: Vec<(String, HashSet<String>)>,
    document_responses: HashMap<String, Response>,
    /// Sessions of out-of-process iframes attached to this page.
    pub child_sessions: HashMap<String, Arc<dyn Session>>,
}

impl PageState {
    pub(crate) fn reached(&self, loader_id: &str, name: &str) -> bool {
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
                if self.lifecycle.len() == 64 {
                    self.lifecycle.remove(0);
                }
                self.lifecycle
                    .push((loader_id.to_owned(), HashSet::from([name.to_owned()])));
            }
        }
    }

    fn frame_mut(&mut self, id: &str) -> &mut FrameData {
        if !self.frames.contains_key(id) {
            self.frame_order.push(id.to_owned());
        }
        self.frames.entry(id.to_owned()).or_default()
    }

    /// Removes a frame and its descendants; returns the ids removed.
    fn remove_frame(&mut self, id: &str) -> Vec<String> {
        let mut removed = vec![id.to_owned()];
        let mut i = 0;
        while i < removed.len() {
            let parent = removed[i].clone();
            removed.extend(
                self.frames
                    .iter()
                    .filter(|(_, f)| f.parent_id.as_deref() == Some(parent.as_str()))
                    .map(|(k, _)| k.clone()),
            );
            i += 1;
        }
        for r in &removed {
            self.frames.remove(r);
            self.contexts.remove(r);
        }
        self.frame_order.retain(|f| !removed.contains(f));
        removed
    }

    pub(crate) fn take_response(&mut self, loader_id: &str) -> Option<Response> {
        self.document_responses.remove(loader_id)
    }
}

/// A browser tab. Cheap to clone.
///
/// Most calls act on the page's main frame: [`Page::goto`], [`Page::locator`] and the
/// `get_by_*` methods are shortcuts for the same calls on [`Page::main_frame`].
///
/// ```no_run
/// # async fn example(page: agito::Page) -> agito::Result<()> {
/// page.goto("https://example.com").await?;
/// let title = page.title().await?;
/// page.get_by_role("link", Some("Learn more")).click().await?;
/// let png = page.screenshot(Default::default()).await?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Page {
    pub(crate) inner: Arc<PageInner>,
}

pub(crate) struct PageInner {
    pub context: BrowserContext,
    pub session: Arc<dyn Session>,
    target_id: String,
    pub(crate) state: Mutex<PageState>,
    pub(crate) events: broadcast::Sender<PageEvent>,
    console_handlers: Mutex<Vec<ConsoleHandler>>,
    error_handlers: Mutex<Vec<ErrorHandler>>,
    dialog_handlers: Mutex<Vec<DialogHandler>>,
    network_handlers: Mutex<NetworkHandlers>,
    popup_handlers: Mutex<Vec<PageHandler>>,
    file_chooser_handlers: Mutex<Vec<FileChooserHandler>>,
    download_handlers: Mutex<Vec<DownloadHandler>>,
    /// Where in-memory uploads are written; removed with the page.
    upload_dir: Mutex<Option<std::path::PathBuf>>,
    opener: Option<Weak<PageInner>>,
    pub(crate) network: Mutex<NetworkState>,
    pub(crate) network_events: broadcast::Sender<NetworkEvent>,
    mouse: Mouse,
    keyboard: Keyboard,
    closed: AtomicBool,
    pumps: Mutex<Vec<JoinHandle<()>>>,
    /// One per pump: a request to be told once the pump has applied every event received so
    /// far. See [`Page::sync_events`].
    barriers: Mutex<Vec<mpsc::UnboundedSender<oneshot::Sender<()>>>>,
}

impl PageInner {
    pub(crate) fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.events.send(PageEvent::Closed);
        self.session.cleanup_subscriptions();
        let children: Vec<_> = self.state.lock().unwrap().child_sessions.drain().collect();
        for (id, child) in children {
            child.cleanup_subscriptions();
            self.context.browser().registry().remove_session(&id);
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Tells handlers and waiters that this page started `download`.
    pub(crate) fn notify_download(&self, download: &Download) {
        let handlers = self.download_handlers.lock().unwrap().clone();
        for handler in handlers {
            handler(download);
        }
        let _ = self.events.send(PageEvent::Download(download.clone()));
    }

    /// Tells handlers and waiters that this page opened `popup`.
    pub(crate) fn notify_popup(&self, popup: &Page) {
        let handlers = self.popup_handlers.lock().unwrap().clone();
        for handler in handlers {
            handler(popup);
        }
        let _ = self.events.send(PageEvent::Popup(popup.clone()));
    }

    /// Calls network handlers and wakes waiters, outside any lock.
    fn dispatch_network(&self, events: Vec<NetworkEvent>) {
        for event in events {
            {
                let handlers = self.network_handlers.lock().unwrap();
                match &event {
                    NetworkEvent::Request(r) => handlers.request.iter().for_each(|h| h(r)),
                    NetworkEvent::Response(r) => handlers.response.iter().for_each(|h| h(r)),
                    NetworkEvent::RequestFinished(r) => handlers.finished.iter().for_each(|h| h(r)),
                    NetworkEvent::RequestFailed(r) => handlers.failed.iter().for_each(|h| h(r)),
                }
            }
            let _ = self.network_events.send(event);
        }
    }

    /// The session that owns `frame_id`.
    pub(crate) fn session_for(&self, frame_id: &str) -> Arc<dyn Session> {
        let state = self.state.lock().unwrap();
        state
            .frames
            .get(frame_id)
            .and_then(|f| f.session_id.as_ref())
            .and_then(|s| state.child_sessions.get(s).cloned())
            .unwrap_or_else(|| self.session.clone())
    }
}

impl Drop for PageInner {
    fn drop(&mut self) {
        if let Some(dir) = self.upload_dir.get_mut().unwrap().take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        for pump in self.pumps.get_mut().unwrap().drain(..) {
            pump.abort();
        }
    }
}

const PAGE_EVENTS: &[&str] = &[
    "Page.lifecycleEvent",
    "Page.frameAttached",
    "Page.frameDetached",
    "Page.frameNavigated",
    "Page.navigatedWithinDocument",
    "Page.javascriptDialogOpening",
    "Runtime.executionContextCreated",
    "Runtime.executionContextDestroyed",
    "Runtime.executionContextsCleared",
    "Runtime.consoleAPICalled",
    "Runtime.exceptionThrown",
    "Network.requestWillBeSent",
    "Network.responseReceived",
    "Network.loadingFinished",
    "Network.loadingFailed",
    "Fetch.requestPaused",
    "Target.attachedToTarget",
    "Target.detachedFromTarget",
    "Page.fileChooserOpened",
    "Inspector.targetCrashed",
];

/// Out-of-process iframes show up as targets of this type.
const IFRAME_TARGET: &str = "iframe";

impl Page {
    pub(crate) fn from_inner(inner: Arc<PageInner>) -> Self {
        Self { inner }
    }

    /// Sets up a page target the browser attached. `initial_url` is the URL the target was
    /// opened for (`about:blank` for `new_page`); `paused` says it waits to be resumed.
    pub(crate) async fn initialize(
        context: BrowserContext,
        session: Arc<dyn Session>,
        target_id: String,
        opener: Option<&Page>,
        initial_url: &str,
        paused: bool,
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
            network_handlers: Mutex::new(NetworkHandlers::default()),
            popup_handlers: Mutex::new(Vec::new()),
            file_chooser_handlers: Mutex::new(Vec::new()),
            download_handlers: Mutex::new(Vec::new()),
            upload_dir: Mutex::new(None),
            opener: opener.map(|o| Arc::downgrade(&o.inner)),
            network: Mutex::new(NetworkState::default()),
            network_events: broadcast::channel(4096).0,
            mouse: Mouse::new(session.clone()),
            keyboard: Keyboard::new(session.clone()),
            closed: AtomicBool::new(false),
            pumps: Mutex::new(Vec::new()),
            barriers: Mutex::new(Vec::new()),
        });
        let page = Page { inner };
        page.start_pump(session.clone(), None);

        // Everything the page needs before it runs goes out as one ordered batch, the resume
        // last. Chrome applies a session's commands in order, but a tab opened without an
        // opener has no renderer until it resumes, so waiting on each answer would deadlock.
        let options = page.inner.context.options().clone();
        let user_agent = match (&options.user_agent, &options.locale) {
            (Some(ua), _) => Some(ua.clone()),
            (None, Some(_)) => Some(
                page.inner
                    .context
                    .browser()
                    .browser_session()
                    .send(&agito_protocol::cdp::browser::GetVersionParams::default())
                    .await?
                    .user_agent,
            ),
            (None, None) => None,
        };
        let scripts = page
            .inner
            .context
            .inner
            .init_scripts
            .lock()
            .unwrap()
            .clone();
        let s = &session;
        let mut batch: Vec<SetupStep<'_>> = vec![
            step(s, cdp_page::EnableParams::default()),
            step(
                s,
                cdp_page::SetLifecycleEventsEnabledParams { enabled: true },
            ),
            step(s, runtime::EnableParams::default()),
            step(s, network::EnableParams::default()),
            // File inputs report to agito instead of opening a native picker (which, in a
            // headed browser, would block until a person answered it).
            step(
                s,
                cdp_page::SetInterceptFileChooserDialogParams {
                    enabled: true,
                    cancel: None,
                },
            ),
        ];
        if let Some(viewport) = options.viewport {
            batch.push(step(s, viewport_params(viewport)));
        }
        if let Some(user_agent) = user_agent {
            batch.push(step(
                s,
                emulation::SetUserAgentOverrideParams {
                    user_agent,
                    accept_language: options.locale.clone(),
                    ..Default::default()
                },
            ));
        }
        if let Some(locale) = &options.locale {
            batch.push(step(
                s,
                emulation::SetLocaleOverrideParams {
                    locale: Some(locale.clone()),
                },
            ));
        }
        if let Some(tz) = &options.timezone_id {
            batch.push(step(
                s,
                emulation::SetTimezoneOverrideParams {
                    timezone_id: tz.clone(),
                },
            ));
        }
        if let Some(scheme) = options.color_scheme_css() {
            batch.push(step(
                s,
                emulation::SetEmulatedMediaParams {
                    media: None,
                    features: Some(vec![emulation::MediaFeature {
                        name: "prefers-color-scheme".into(),
                        value: scheme.into(),
                    }]),
                },
            ));
        }
        if options.ignore_https_errors {
            batch.push(step(
                s,
                security::SetIgnoreCertificateErrorsParams { ignore: true },
            ));
        }
        if !options.extra_http_headers.is_empty() {
            batch.push(step(
                s,
                network::SetExtraHTTPHeadersParams {
                    headers: serde_json::to_value(&options.extra_http_headers).unwrap(),
                },
            ));
        }
        for script in scripts {
            batch.push(step(
                s,
                cdp_page::AddScriptToEvaluateOnNewDocumentParams {
                    source: script,
                    ..Default::default()
                },
            ));
        }
        // Routes registered on the context before this page existed apply to it too.
        batch.push(Box::pin(page.sync_fetch_on(s)));
        // Out-of-process iframes attach as targets; flatten them onto this connection.
        batch.push(step(s, auto_attach_params()));
        if paused {
            batch.push(step(s, runtime::RunIfWaitingForDebuggerParams {}));
        }
        futures::future::try_join_all(batch).await?;

        let tree = session
            .send(&cdp_page::GetFrameTreeParams::default())
            .await?;
        {
            let mut state = page.inner.state.lock().unwrap();
            state.main_frame_id = tree.frame_tree.frame.id.clone();
            record_tree(&mut state, &tree.frame_tree, None);
            // A blank new tab's document loaded before lifecycle events were on. A popup is
            // still about to navigate, so its blank document does not count.
            let loader = tree.frame_tree.frame.loader_id.clone();
            if initial_url == "about:blank" && !state.lifecycle.iter().any(|(l, _)| *l == loader) {
                for name in ["DOMContentLoaded", "load", "networkIdle"] {
                    state.record(&loader, name);
                }
            }
        }
        Ok(page)
    }

    fn start_pump(&self, session: Arc<dyn Session>, child_session_id: Option<String>) {
        // Subscribe before enabling domains so nothing they announce is missed.
        let stream = session.subscribe(PAGE_EVENTS);
        let (barrier, barriers) = mpsc::unbounded_channel();
        let pump = tokio::spawn(event_pump(
            Arc::downgrade(&self.inner),
            session,
            child_session_id,
            stream,
            barriers,
        ));
        self.inner.pumps.lock().unwrap().push(pump);
        self.inner.barriers.lock().unwrap().push(barrier);
    }

    /// The context this page belongs to.
    pub fn context(&self) -> &BrowserContext {
        &self.inner.context
    }

    /// The protocol's id for this page's target.
    pub fn target_id(&self) -> &str {
        &self.inner.target_id
    }

    /// The top-level frame.
    pub fn main_frame(&self) -> Frame {
        let id = self.inner.state.lock().unwrap().main_frame_id.clone();
        Frame::new(self.clone(), id)
    }

    /// Every frame attached to the page, the main frame first.
    pub fn frames(&self) -> Vec<Frame> {
        let state = self.inner.state.lock().unwrap();
        let mut ids = vec![state.main_frame_id.clone()];
        ids.extend(
            state
                .frame_order
                .iter()
                .filter(|id| **id != state.main_frame_id && state.frames.contains_key(*id))
                .cloned(),
        );
        drop(state);
        ids.into_iter()
            .map(|id| Frame::new(self.clone(), id))
            .collect()
    }

    /// The first frame whose `name` attribute is `name`.
    pub fn frame(&self, name: &str) -> Option<Frame> {
        self.frames().into_iter().find(|f| f.name() == name)
    }

    /// The first frame whose URL matches.
    pub fn frame_by_url(&self, matcher: impl Into<UrlMatcher>) -> Option<Frame> {
        let matcher = matcher.into();
        self.frames()
            .into_iter()
            .find(|f| matcher.matches(&f.url()))
    }

    pub(crate) fn frame_by_id(&self, id: &str) -> Option<Frame> {
        self.inner
            .state
            .lock()
            .unwrap()
            .frames
            .contains_key(id)
            .then(|| Frame::new(self.clone(), id.to_owned()))
    }

    /// Locates elements inside the iframe matched by `selector`. The iframe is found again
    /// each time, so the locator survives the iframe being reloaded or replaced.
    pub fn frame_locator(&self, selector: &str) -> FrameLocator {
        FrameLocator::new(self.locator(selector))
    }

    /// The main frame's current URL.
    pub fn url(&self) -> String {
        self.main_frame().url()
    }

    /// Whether the page has been closed.
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    /// The page's mouse.
    pub fn mouse(&self) -> &Mouse {
        &self.inner.mouse
    }

    /// The page's keyboard.
    pub fn keyboard(&self) -> &Keyboard {
        &self.inner.keyboard
    }

    pub(crate) fn ensure_open(&self) -> Result<()> {
        if self.is_closed() {
            return Err(Error::target_closed(
                "page",
                Some(&self.inner.target_id),
                "The page has been closed.",
            ));
        }
        Ok(())
    }

    pub(crate) fn resolve_url(&self, url: &str) -> String {
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

    pub(crate) fn navigation_timeout(&self, options: &NavigateOptions) -> Duration {
        options
            .timeout
            .unwrap_or(self.inner.context.options().navigation_timeout)
    }

    /// Navigates to `url` and waits for the `load` event. Returns the main document's response,
    /// or `None` for navigations without one (same-document, `about:blank`).
    pub async fn goto(&self, url: &str) -> Result<Option<Response>> {
        self.main_frame().goto(url).await
    }

    /// [`Page::goto`] with options.
    pub async fn goto_with(&self, url: &str, options: NavigateOptions) -> Result<Option<Response>> {
        self.main_frame().goto_with(url, options).await
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

    /// Waits for a main-frame navigation started without knowing its document: the next
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
        let main = self.inner.state.lock().unwrap().main_frame_id.clone();
        let deadline = Instant::now() + timeout;
        let loader_id = loop {
            match self.next_event(events, deadline).await {
                Some(PageEvent::Navigated {
                    frame_id,
                    loader_id,
                    ..
                }) if frame_id == main => match loader_id {
                    Some(l) => break l,
                    // A navigation within the document (a hash change) loads nothing more.
                    None => return Ok(None),
                },
                Some(PageEvent::Closed) => return Err(self.closed_error()),
                Some(_) => continue,
                None => return Err(navigation_timeout(url, timeout)),
            }
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        self.wait_for_lifecycle(events, &loader_id, name, url, remaining)
            .await?;
        Ok(self.inner.state.lock().unwrap().take_response(&loader_id))
    }

    pub(crate) async fn next_event(
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

    /// Waits until the document `loader_id` has fired lifecycle event `name`.
    pub(crate) async fn wait_for_lifecycle(
        &self,
        events: &mut broadcast::Receiver<PageEvent>,
        loader_id: &str,
        name: &str,
        url: &str,
        timeout: Duration,
    ) -> Result<()> {
        if self.inner.state.lock().unwrap().reached(loader_id, name) {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        loop {
            match self.next_event(events, deadline).await {
                Some(PageEvent::Lifecycle {
                    loader_id: l,
                    name: n,
                    ..
                }) if n == name && l == loader_id => return Ok(()),
                Some(PageEvent::Closed) => return Err(self.closed_error()),
                Some(_) => {}
                None => return Err(navigation_timeout(url, timeout)),
            }
        }
    }

    pub(crate) fn closed_error(&self) -> Error {
        Error::target_closed(
            "page",
            Some(&self.inner.target_id),
            "The page closed while waiting for it.",
        )
    }

    /// Waits until the main frame's document has reached `state`.
    pub async fn wait_for_load_state(
        &self,
        state: WaitUntil,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.main_frame().wait_for_load_state(state, timeout).await
    }

    /// Waits until the main frame's URL matches.
    pub async fn wait_for_url(
        &self,
        matcher: impl Into<UrlMatcher>,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.main_frame().wait_for_url(matcher, timeout).await
    }

    /// Sleeps. Prefer waiting for a condition.
    pub async fn wait_for_timeout(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    /// Evaluates a JavaScript expression in the main frame and decodes its value. A function
    /// expression (`() => document.title`) is called; a promise is awaited.
    pub async fn evaluate<T: DeserializeOwned>(&self, expression: &str) -> Result<T> {
        self.main_frame().evaluate(expression).await
    }

    /// Calls a JavaScript function in the main frame with one serializable argument.
    pub async fn evaluate_with<T: DeserializeOwned, A: Serialize>(
        &self,
        function: &str,
        arg: &A,
    ) -> Result<T> {
        self.main_frame().evaluate_with(function, arg).await
    }

    /// The document's title.
    pub async fn title(&self) -> Result<String> {
        self.main_frame().title().await
    }

    /// The full HTML of the document, doctype included.
    pub async fn content(&self) -> Result<String> {
        self.main_frame().content().await
    }

    /// Replaces the document with `html`.
    pub async fn set_content(&self, html: &str) -> Result<()> {
        self.main_frame().set_content(html).await
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
        self.inner.session.send(&viewport_params(viewport)).await?;
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

    /// Calls `handler` for every console message, from any frame.
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

    /// The directory in-memory uploads are written to, created on first use.
    pub(crate) fn upload_dir(&self) -> Result<std::path::PathBuf> {
        let mut dir = self.inner.upload_dir.lock().unwrap();
        if let Some(d) = &*dir {
            return Ok(d.clone());
        }
        let d = crate::files::temp_dir("upload")?;
        *dir = Some(d.clone());
        Ok(d)
    }

    /// Handles file pickers the page opens. The handler should answer with
    /// [`FileChooser::set_files`]; a chooser nobody answers stays empty.
    pub fn on_file_chooser<F, Fut>(&self, handler: F)
    where
        F: Fn(FileChooser) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.inner
            .file_chooser_handlers
            .lock()
            .unwrap()
            .push(Arc::new(move |c| Box::pin(handler(c))));
    }

    /// Waits for the page to open a file picker. Starts listening as soon as it is called,
    /// so create it before the click that opens the picker:
    ///
    /// ```no_run
    /// # async fn demo(page: agito::Page) -> agito::Result<()> {
    /// let chooser = page.wait_for_file_chooser(None);
    /// page.get_by_text("Upload").click().await?;
    /// chooser.await?.set_files("report.pdf").await?;
    /// # Ok(()) }
    /// ```
    pub fn wait_for_file_chooser(
        &self,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<FileChooser>> + Send + 'static {
        self.page_event_waiter(timeout, "file chooser", |e| match e {
            PageEvent::FileChooser(c) => Some(c.clone()),
            _ => None,
        })
    }

    /// Calls `handler` for every download the page starts.
    pub fn on_download(&self, handler: impl Fn(&Download) + Send + Sync + 'static) {
        self.inner
            .download_handlers
            .lock()
            .unwrap()
            .push(Arc::new(handler));
    }

    /// Waits for the page to start a download. Starts listening as soon as it is called, so
    /// create it before the action that downloads:
    ///
    /// ```no_run
    /// # async fn demo(page: agito::Page) -> agito::Result<()> {
    /// let download = page.wait_for_download(None);
    /// page.get_by_text("Export CSV").click().await?;
    /// download.await?.save_as("export.csv").await?;
    /// # Ok(()) }
    /// ```
    pub fn wait_for_download(
        &self,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Download>> + Send + 'static {
        self.page_event_waiter(timeout, "download", |e| match e {
            PageEvent::Download(d) => Some(d.clone()),
            _ => None,
        })
    }

    fn page_event_waiter<T: Send + 'static>(
        &self,
        timeout: Option<Duration>,
        what: &'static str,
        pick: impl Fn(&PageEvent) -> Option<T> + Send + Sync + 'static,
    ) -> impl Future<Output = Result<T>> + Send + 'static {
        let mut events = self.inner.events.subscribe();
        let timeout = timeout.unwrap_or(self.inner.context.options().navigation_timeout);
        let page = self.clone();
        async move {
            let wait = async {
                loop {
                    match events.recv().await {
                        Ok(PageEvent::Closed) | Err(broadcast::error::RecvError::Closed) => {
                            return Err(page.closed_error());
                        }
                        Ok(e) => {
                            if let Some(found) = pick(&e) {
                                return Ok(found);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                    }
                }
            };
            tokio::time::timeout(timeout, wait)
                .await
                .unwrap_or_else(|_| {
                    Err(ErrorKind::WaitTimeout {
                        condition: what.into(),
                        timeout,
                        last_evaluated_value: None,
                        message: format!("No {what} within {}ms.", timeout.as_millis()),
                    }
                    .into())
                })
        }
    }

    /// The page that opened this one, if it was opened by a page and that page is still open.
    pub fn opener(&self) -> Option<Page> {
        let opener = self.inner.opener.as_ref()?.upgrade()?;
        (!opener.is_closed()).then(|| Page::from_inner(opener))
    }

    /// Calls `handler` for every page this page opens (`window.open`, `target=_blank`).
    pub fn on_popup(&self, handler: impl Fn(&Page) + Send + Sync + 'static) {
        self.inner
            .popup_handlers
            .lock()
            .unwrap()
            .push(Arc::new(handler));
    }

    /// Waits for this page to open another. Starts listening as soon as it is called, so
    /// create it before the action that opens the popup:
    ///
    /// ```no_run
    /// # async fn demo(page: agito::Page) -> agito::Result<()> {
    /// let popup = page.wait_for_popup(None);
    /// page.get_by_text("Open in new tab").click().await?;
    /// let popup = popup.await?;
    /// popup.wait_for_load_state(agito::WaitUntil::Load, None).await?;
    /// # Ok(()) }
    /// ```
    pub fn wait_for_popup(
        &self,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Page>> + Send + 'static {
        let mut events = self.inner.events.subscribe();
        let timeout = timeout.unwrap_or(self.inner.context.options().navigation_timeout);
        let page = self.clone();
        async move {
            let wait = async {
                loop {
                    match events.recv().await {
                        Ok(PageEvent::Popup(popup)) => return Ok(popup),
                        Ok(PageEvent::Closed) | Err(broadcast::error::RecvError::Closed) => {
                            return Err(page.closed_error());
                        }
                        _ => {}
                    }
                }
            };
            tokio::time::timeout(timeout, wait)
                .await
                .unwrap_or_else(|_| {
                    Err(ErrorKind::WaitTimeout {
                        condition: "popup".into(),
                        timeout,
                        last_evaluated_value: None,
                        message: format!(
                            "The page opened no popup within {}ms.",
                            timeout.as_millis()
                        ),
                    }
                    .into())
                })
        }
    }

    /// Calls `handler` for every request the page (or any of its frames) starts.
    pub fn on_request(&self, handler: impl Fn(&Request) + Send + Sync + 'static) {
        self.inner
            .network_handlers
            .lock()
            .unwrap()
            .request
            .push(Arc::new(handler));
    }

    /// Calls `handler` for every response, once its headers arrive.
    pub fn on_response(&self, handler: impl Fn(&Response) + Send + Sync + 'static) {
        self.inner
            .network_handlers
            .lock()
            .unwrap()
            .response
            .push(Arc::new(handler));
    }

    /// Calls `handler` for every request that finished loading.
    pub fn on_request_finished(&self, handler: impl Fn(&Request) + Send + Sync + 'static) {
        self.inner
            .network_handlers
            .lock()
            .unwrap()
            .finished
            .push(Arc::new(handler));
    }

    /// Calls `handler` for every request that failed (see [`Request::failure`]).
    pub fn on_request_failed(&self, handler: impl Fn(&Request) + Send + Sync + 'static) {
        self.inner
            .network_handlers
            .lock()
            .unwrap()
            .failed
            .push(Arc::new(handler));
    }

    /// Waits for a request whose URL matches. Starts listening as soon as it is called, so
    /// create the future before triggering the request:
    ///
    /// ```no_run
    /// # async fn demo(page: agito::Page) -> agito::Result<()> {
    /// let request = page.wait_for_request("**/api/search*", None);
    /// page.locator("button#search").click().await?;
    /// let request = request.await?;
    /// # Ok(()) }
    /// ```
    pub fn wait_for_request(
        &self,
        matcher: impl Into<UrlMatcher>,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Request>> + Send + 'static {
        let matcher = matcher.into();
        self.network_waiter(timeout, "request", matcher.describe(), move |e| match e {
            NetworkEvent::Request(r) if matcher.matches(r.url()) => Some(r.clone()),
            _ => None,
        })
    }

    /// Waits for a response whose URL matches. Starts listening as soon as it is called; see
    /// [`Page::wait_for_request`].
    pub fn wait_for_response(
        &self,
        matcher: impl Into<UrlMatcher>,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<Response>> + Send + 'static {
        let matcher = matcher.into();
        self.network_waiter(timeout, "response", matcher.describe(), move |e| match e {
            NetworkEvent::Response(r) if matcher.matches(r.url()) => Some(r.clone()),
            _ => None,
        })
    }

    fn network_waiter<T: Send + 'static>(
        &self,
        timeout: Option<Duration>,
        what: &'static str,
        description: String,
        mut pick: impl FnMut(&NetworkEvent) -> Option<T> + Send + 'static,
    ) -> impl Future<Output = Result<T>> + Send + 'static {
        let mut events = self.inner.network_events.subscribe();
        let mut closed = self.inner.events.subscribe();
        let timeout = timeout.unwrap_or(self.inner.context.options().navigation_timeout);
        let page = self.clone();
        async move {
            let wait = async {
                loop {
                    tokio::select! {
                        event = events.recv() => match event {
                            Ok(e) => if let Some(found) = pick(&e) { return Ok(found) },
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return Err(page.closed_error()),
                        },
                        event = closed.recv() => {
                            if matches!(event, Ok(PageEvent::Closed) | Err(broadcast::error::RecvError::Closed)) {
                                return Err(page.closed_error());
                            }
                        }
                    }
                }
            };
            tokio::time::timeout(timeout, wait)
                .await
                .unwrap_or_else(|_| {
                    Err(ErrorKind::WaitTimeout {
                        condition: format!("{what} matching {description}"),
                        timeout,
                        last_evaluated_value: None,
                        message: format!(
                            "Timed out after {}ms waiting for a {what} matching {description}.",
                            timeout.as_millis()
                        ),
                    }
                    .into())
                })
        }
    }

    /// Waits until no request has been in flight for `idle` (500ms is typical). Useful after
    /// an action that starts background requests.
    pub async fn wait_for_network_idle(
        &self,
        idle: Duration,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.ensure_open()?;
        let timeout = timeout.unwrap_or(self.inner.context.options().navigation_timeout);
        let deadline = Instant::now() + timeout;
        let mut events = self.inner.network_events.subscribe();
        loop {
            let in_flight = self.inner.network.lock().unwrap().requests.len();
            let now = Instant::now();
            if now >= deadline {
                return Err(ErrorKind::WaitTimeout {
                    condition: "network idle".into(),
                    timeout,
                    last_evaluated_value: Some(format!("{in_flight} request(s) in flight")),
                    message: format!(
                        "The network was still busy after {}ms ({in_flight} request(s) in flight).",
                        timeout.as_millis()
                    ),
                }
                .into());
            }
            if in_flight == 0 {
                // Quiet so far: idle once `idle` passes with no new request.
                let quiet_until = (now + idle).min(deadline);
                let started = loop {
                    match tokio::time::timeout_at(quiet_until.into(), events.recv()).await {
                        Err(_) => break false,
                        Ok(Ok(NetworkEvent::Request(_))) => break true,
                        Ok(Err(broadcast::error::RecvError::Closed)) => {
                            return Err(self.closed_error());
                        }
                        Ok(_) => {}
                    }
                };
                if !started && quiet_until < deadline {
                    return Ok(());
                }
            } else {
                let _ = tokio::time::timeout_at(deadline.into(), events.recv()).await;
            }
        }
    }

    /// Intercepts requests whose URL matches `pattern` (a glob like `**/api/**`, a regex, or a
    /// plain string any matching URL contains). The handler fulfills, continues or aborts the
    /// [`Route`](crate::network::Route); if it does none of these the request goes through
    /// unchanged. The most recently added matching route handles a request.
    ///
    /// ```no_run
    /// # async fn demo(page: agito::Page) -> agito::Result<()> {
    /// use agito::network::FulfillOptions;
    /// page.route("**/api/user", |route| async move {
    ///     let _ = route.fulfill(FulfillOptions::json(&serde_json::json!({ "name": "Ada" }))).await;
    /// })
    /// .await?;
    /// # Ok(()) }
    /// ```
    pub async fn route<F, Fut>(
        &self,
        pattern: impl Into<RoutePattern>,
        handler: F,
    ) -> Result<RouteId>
    where
        F: Fn(Arc<crate::network::Route>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let entry = make_route_entry(pattern.into(), handler);
        let id = RouteId(entry.id);
        self.inner.network.lock().unwrap().routes.push(entry);
        self.sync_fetch().await?;
        Ok(id)
    }

    /// Removes a route added with [`Page::route`].
    pub async fn unroute(&self, id: RouteId) -> Result<()> {
        self.inner
            .network
            .lock()
            .unwrap()
            .routes
            .retain(|r| r.id != id.0);
        self.sync_fetch().await
    }

    /// Removes every route added with [`Page::route`].
    pub async fn unroute_all(&self) -> Result<()> {
        self.inner.network.lock().unwrap().routes.clear();
        self.sync_fetch().await
    }

    /// Turns request interception on or off in every session of the page, to match whether
    /// any page or context route exists.
    pub(crate) async fn sync_fetch(&self) -> Result<()> {
        if self.is_closed() {
            return Ok(());
        }
        let mut sessions = vec![self.inner.session.clone()];
        sessions.extend(
            self.inner
                .state
                .lock()
                .unwrap()
                .child_sessions
                .values()
                .cloned(),
        );
        for session in sessions {
            match self.sync_fetch_on(&session).await {
                Err(e) if e.is_browser_lost() => {}
                other => other?,
            }
        }
        Ok(())
    }

    pub(crate) async fn sync_fetch_on(&self, session: &Arc<dyn Session>) -> Result<()> {
        let want = !self.inner.network.lock().unwrap().routes.is_empty()
            || !self.inner.context.inner.routes.lock().unwrap().is_empty();
        let key = session.session_id().unwrap_or_default().to_owned();
        let enabled = self
            .inner
            .network
            .lock()
            .unwrap()
            .fetch_sessions
            .contains(&key);
        if want && !enabled {
            session
                .send(&fetch::EnableParams {
                    patterns: Some(vec![fetch::RequestPattern {
                        url_pattern: Some("*".into()),
                        resource_type: None,
                        request_stage: Some(fetch::RequestStage::Request),
                    }]),
                    handle_auth_requests: None,
                })
                .await?;
            self.inner
                .network
                .lock()
                .unwrap()
                .fetch_sessions
                .insert(key);
        } else if !want && enabled {
            session.send(&fetch::DisableParams::default()).await?;
            self.inner
                .network
                .lock()
                .unwrap()
                .fetch_sessions
                .remove(&key);
        }
        Ok(())
    }

    /// Starts recording a HAR log of everything the page loads.
    pub fn start_har(&self) {
        self.inner.network.lock().unwrap().har = Some(Default::default());
    }

    /// Stops recording and returns the HAR log as JSON, or `None` if no recording was running.
    pub fn stop_har(&self) -> Option<Value> {
        self.inner
            .network
            .lock()
            .unwrap()
            .har
            .take()
            .map(|h| h.to_har())
    }

    /// Stops recording and writes the HAR log to `path`.
    pub fn stop_har_to(&self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let path = path.as_ref();
        let har = self.stop_har().ok_or_else(|| {
            Error::invalid_argument("no HAR recording is running; call start_har first")
        })?;
        let text = serde_json::to_string_pretty(&har).expect("HAR serializes");
        std::fs::write(path, text).map_err(|e| Error::io(format!("writing {}", path.display()), e))
    }

    /// A locator for `selector` in the main frame. Selectors are CSS by default; a `prefix=`
    /// picks another strategy (`text=`, `xpath=`, `role=`, `data-testid=`, ...).
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::new(self.clone(), selector)
    }

    /// A locator for the element the browser knows by `backend_node_id` in the main frame, as
    /// [`AxNode::backend_node_id`](crate::accessibility::AxNode) gives it.
    pub fn locator_by_backend_node_id(&self, backend_node_id: i64) -> Locator {
        self.locator(&format!("backend-node-id={backend_node_id}"))
    }

    /// Waits until the page has applied every event the browser sent before now.
    ///
    /// Answers to commands are handed back as they arrive, but events go through each pump's
    /// queue, so a command's caller can run ahead of events the browser sent first: a script
    /// that adds an iframe returns before the page has heard the frame's name. Each pump
    /// prefers its events to barrier requests, and every earlier event is already queued, so
    /// a pump answering has applied them all. The wait involves no browser round trip; a pump
    /// slow to answer is not waited for beyond a second.
    pub(crate) async fn sync_events(&self) {
        let senders = self.inner.barriers.lock().unwrap().clone();
        let answers: Vec<_> = senders
            .iter()
            .filter_map(|barrier| {
                let (tx, rx) = oneshot::channel();
                barrier.send(tx).ok().map(|()| rx)
            })
            .collect();
        let _ =
            tokio::time::timeout(Duration::from_secs(1), futures::future::join_all(answers)).await;
    }

    pub(crate) async fn fire_hooks_before_navigation(&self, url: &str) {
        for hook in &self.inner.context.inner.hooks {
            if let Err(e) = hook.before_navigation(self, url).await {
                tracing::warn!(error = %e, "lifecycle hook before_navigation failed");
            }
        }
    }

    pub(crate) async fn fire_hooks_after_navigation(&self, response: Option<&Response>) {
        for hook in &self.inner.context.inner.hooks {
            if let Err(e) = hook.after_navigation(self, response).await {
                tracing::warn!(error = %e, "lifecycle hook after_navigation failed");
            }
        }
    }
}

crate::frame::impl_get_by!(Page);

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("target_id", &self.inner.target_id)
            .field("url", &self.url())
            .finish()
    }
}

/// Turns on the domains a page or iframe session reports through.
async fn enable_session(session: &Arc<dyn Session>) -> Result<()> {
    session.send(&cdp_page::EnableParams::default()).await?;
    session
        .send(&cdp_page::SetLifecycleEventsEnabledParams { enabled: true })
        .await?;
    session.send(&runtime::EnableParams::default()).await?;
    session.send(&network::EnableParams::default()).await?;
    Ok(())
}

fn auto_attach_params() -> target::SetAutoAttachParams {
    target::SetAutoAttachParams {
        auto_attach: true,
        wait_for_debugger_on_start: false,
        flatten: Some(true),
        ..Default::default()
    }
}

async fn attach_iframes(session: &Arc<dyn Session>) -> Result<()> {
    session.send(&auto_attach_params()).await?;
    Ok(())
}

fn viewport_params(viewport: Viewport) -> emulation::SetDeviceMetricsOverrideParams {
    emulation::SetDeviceMetricsOverrideParams {
        width: viewport.width.into(),
        height: viewport.height.into(),
        device_scale_factor: 1.0,
        mobile: false,
        ..Default::default()
    }
}

type SetupStep<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// One command of a page's setup batch.
fn step<'a, C>(session: &'a Arc<dyn Session>, command: C) -> SetupStep<'a>
where
    C: agito_protocol::Command + Send + Sync + 'a,
    C::Response: Send,
{
    Box::pin(async move { session.send(&command).await.map(|_| ()) })
}

fn record_tree(state: &mut PageState, node: &cdp_page::FrameTree, session_id: Option<&str>) {
    let f = &node.frame;
    let data = state.frame_mut(&f.id);
    data.parent_id = f.parent_id.clone();
    data.name = f.name.clone().unwrap_or_default();
    data.url = f.url.clone();
    data.loader_id = Some(f.loader_id.clone());
    data.session_id = session_id.map(str::to_owned);
    for child in node.child_frames.iter().flatten() {
        record_tree(state, child, session_id);
    }
}

/// Takes over an out-of-process iframe's session: tracks its frames and events.
async fn adopt_iframe(page: Page, session_id: String, frame_id: String) {
    let registry = page.inner.context.browser().registry().clone();
    let session = registry.create_session(&session_id);
    {
        let mut state = page.inner.state.lock().unwrap();
        state
            .child_sessions
            .insert(session_id.clone(), session.clone());
        state.frame_mut(&frame_id).session_id = Some(session_id.clone());
    }
    page.start_pump(session.clone(), Some(session_id.clone()));
    let setup = async {
        enable_session(&session).await?;
        page.sync_fetch_on(&session).await?;
        attach_iframes(&session).await?;
        let tree = session
            .send(&cdp_page::GetFrameTreeParams::default())
            .await?;
        let mut state = page.inner.state.lock().unwrap();
        // The iframe's own parent link is known to the page that owns it, not to the iframe.
        let parent = state
            .frames
            .get(&frame_id)
            .and_then(|f| f.parent_id.clone());
        record_tree(&mut state, &tree.frame_tree, Some(&session_id));
        if let Some(f) = state.frames.get_mut(&frame_id)
            && f.parent_id.is_none()
        {
            f.parent_id = parent;
        }
        Ok::<_, Error>(())
    };
    if let Err(e) = setup.await
        && !e.is_browser_lost()
    {
        tracing::warn!(frame = %frame_id, error = %e, "an out-of-process iframe could not be adopted");
    }
}

fn decode_base64(data: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| Error::other(format!("the browser sent invalid base64: {e}")))
}

pub(crate) fn navigation_timeout(url: &str, timeout: Duration) -> Error {
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

/// Handles one session's events. `child` is the session id for an out-of-process iframe, or
/// `None` for the page's own session (whose end means the page is gone).
async fn event_pump(
    page: Weak<PageInner>,
    session: Arc<dyn Session>,
    child: Option<String>,
    mut stream: crate::transport::EventStream,
    mut barriers: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
) {
    loop {
        // Events first: a barrier is answered only once none is waiting.
        let event = tokio::select! {
            biased;
            event = stream.next() => event,
            Some(barrier) = barriers.recv() => {
                let _ = barrier.send(());
                continue;
            }
        };
        let Some(event) = event else { break };
        let Some(inner) = page.upgrade() else { return };
        let params = event.params;
        match event.method.as_str() {
            "Page.lifecycleEvent" => {
                let Ok(e) = serde_json::from_value::<cdp_page::LifecycleEventEvent>(params) else {
                    continue;
                };
                {
                    let mut state = inner.state.lock().unwrap();
                    if e.name == "init" {
                        state.frame_mut(&e.frame_id).loader_id = Some(e.loader_id.clone());
                    }
                    state.record(&e.loader_id, &e.name);
                }
                let _ = inner.events.send(PageEvent::Lifecycle {
                    frame_id: e.frame_id,
                    loader_id: e.loader_id,
                    name: e.name,
                });
            }
            "Page.frameAttached" => {
                let (Some(id), Some(parent)) =
                    (params["frameId"].as_str(), params["parentFrameId"].as_str())
                else {
                    continue;
                };
                let mut state = inner.state.lock().unwrap();
                let frame = state.frame_mut(id);
                frame.parent_id = Some(parent.to_owned());
                if frame.session_id.is_none() {
                    frame.session_id = child.clone();
                }
            }
            "Page.frameDetached" => {
                let Some(id) = params["frameId"].as_str() else {
                    continue;
                };
                // A frame moving into its own process detaches with "swap"; it lives on.
                if params["reason"].as_str() == Some("swap") {
                    continue;
                }
                let removed = inner.state.lock().unwrap().remove_frame(id);
                for frame_id in removed {
                    let _ = inner.events.send(PageEvent::FrameDetached { frame_id });
                }
            }
            "Page.frameNavigated" => {
                let Ok(e) = serde_json::from_value::<cdp_page::FrameNavigatedEvent>(params) else {
                    continue;
                };
                let f = e.frame;
                {
                    let mut state = inner.state.lock().unwrap();
                    if f.parent_id.is_none() && child.is_none() {
                        state.main_frame_id = f.id.clone();
                    }
                    let data = state.frame_mut(&f.id);
                    if f.parent_id.is_some() {
                        data.parent_id = f.parent_id.clone();
                    }
                    data.name = f.name.clone().unwrap_or_default();
                    data.url = f.url.clone();
                    data.loader_id = Some(f.loader_id.clone());
                    // The session that commits a frame's document is the one that owns it.
                    data.session_id = child.clone();
                }
                let _ = inner.events.send(PageEvent::Navigated {
                    frame_id: f.id,
                    url: f.url,
                    loader_id: Some(f.loader_id),
                });
            }
            "Page.navigatedWithinDocument" => {
                let (Some(id), Some(url)) = (params["frameId"].as_str(), params["url"].as_str())
                else {
                    continue;
                };
                inner.state.lock().unwrap().frame_mut(id).url = url.to_owned();
                let _ = inner.events.send(PageEvent::Navigated {
                    frame_id: id.to_owned(),
                    url: url.to_owned(),
                    loader_id: None,
                });
            }
            "Runtime.executionContextCreated" => {
                let Ok(e) = serde_json::from_value::<runtime::ExecutionContextCreatedEvent>(params)
                else {
                    continue;
                };
                let aux = e.context.aux_data.unwrap_or(Value::Null);
                let (Some(frame_id), true) = (
                    aux["frameId"].as_str(),
                    aux["isDefault"].as_bool().unwrap_or(false),
                ) else {
                    continue;
                };
                inner
                    .state
                    .lock()
                    .unwrap()
                    .contexts
                    .insert(frame_id.to_owned(), (e.context.id, child.clone()));
                let _ = inner.events.send(PageEvent::ContextCreated);
            }
            "Runtime.executionContextDestroyed" => {
                let Some(id) = params["executionContextId"].as_i64() else {
                    continue;
                };
                inner
                    .state
                    .lock()
                    .unwrap()
                    .contexts
                    .retain(|_, (ctx, s)| !(*ctx == id && *s == child));
            }
            "Runtime.executionContextsCleared" => {
                inner
                    .state
                    .lock()
                    .unwrap()
                    .contexts
                    .retain(|_, (_, s)| *s != child);
            }
            "Target.attachedToTarget" => {
                let Ok(e) = serde_json::from_value::<target::AttachedToTargetEvent>(params) else {
                    continue;
                };
                if e.target_info.r#type == IFRAME_TARGET {
                    // An out-of-process iframe's target id is its frame id.
                    tokio::spawn(adopt_iframe(
                        Page::from_inner(inner.clone()),
                        e.session_id,
                        e.target_info.target_id,
                    ));
                }
            }
            "Target.detachedFromTarget" => {
                let Some(session_id) = params["sessionId"].as_str() else {
                    continue;
                };
                let removed = {
                    let mut state = inner.state.lock().unwrap();
                    state
                        .contexts
                        .retain(|_, (_, s)| s.as_deref() != Some(session_id));
                    state.child_sessions.remove(session_id)
                };
                if let Some(s) = removed {
                    s.cleanup_subscriptions();
                    inner
                        .context
                        .browser()
                        .registry()
                        .remove_session(session_id);
                }
            }
            "Network.requestWillBeSent" => {
                let Ok(e) = serde_json::from_value::<network::RequestWillBeSentEvent>(params)
                else {
                    continue;
                };
                let mut out = Vec::new();
                {
                    let mut net = inner.network.lock().unwrap();
                    // A redirect reuses the request id: the old request ends with its response.
                    if let (Some(redirect), Some(previous)) =
                        (&e.redirect_response, net.requests.remove(&e.request_id))
                    {
                        let data = ResponseData::from_cdp(redirect, &e.request_id, session.clone());
                        previous.set_response(data.clone());
                        previous.finish(RequestStateUpdate::Finished);
                        out.push(NetworkEvent::Response(Response::new(
                            data,
                            Some(previous.clone()),
                        )));
                        out.push(NetworkEvent::RequestFinished(previous));
                    }
                    let request = Request::from_cdp(
                        e.request_id.clone(),
                        &e.request,
                        e.r#type,
                        e.frame_id.clone(),
                        Arc::downgrade(&inner),
                    );
                    net.requests.insert(e.request_id.clone(), request.clone());
                    if let Some(har) = &mut net.har {
                        har.on_request(&e);
                    }
                    out.push(NetworkEvent::Request(request));
                }
                inner.dispatch_network(out);
            }
            "Network.responseReceived" => {
                let Ok(e) = serde_json::from_value::<network::ResponseReceivedEvent>(params) else {
                    continue;
                };
                let data = ResponseData::from_cdp(&e.response, &e.request_id, session.clone());
                let response = {
                    let mut net = inner.network.lock().unwrap();
                    if let Some(har) = &mut net.har {
                        har.on_response(&e);
                    }
                    let request = net.requests.get(&e.request_id).cloned();
                    if let Some(r) = &request {
                        r.set_response(data.clone());
                    }
                    Response::new(data, request)
                };
                if e.r#type == network::ResourceType::Document {
                    inner
                        .state
                        .lock()
                        .unwrap()
                        .document_responses
                        .insert(e.loader_id.clone(), response.clone());
                }
                inner.dispatch_network(vec![NetworkEvent::Response(response)]);
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                let Some(id) = params["requestId"].as_str() else {
                    continue;
                };
                let failed = event.method == "Network.loadingFailed";
                let error = params["errorText"].as_str().unwrap_or("failed").to_owned();
                let request = {
                    let mut net = inner.network.lock().unwrap();
                    if let Some(har) = &mut net.har {
                        har.on_end(
                            id,
                            params["timestamp"].as_f64().unwrap_or(0.0),
                            params["encodedDataLength"].as_f64().unwrap_or(0.0),
                            failed.then_some(error.as_str()),
                        );
                    }
                    net.requests.remove(id)
                };
                let Some(request) = request else { continue };
                if failed {
                    request.finish(RequestStateUpdate::Failed(error));
                    inner.dispatch_network(vec![NetworkEvent::RequestFailed(request)]);
                } else {
                    request.finish(RequestStateUpdate::Finished);
                    inner.dispatch_network(vec![NetworkEvent::RequestFinished(request)]);
                }
            }
            "Fetch.requestPaused" => {
                let Ok(e) = serde_json::from_value::<fetch::RequestPausedEvent>(params) else {
                    continue;
                };
                let route = {
                    let page_routes = inner.network.lock().unwrap().routes.clone();
                    let context_routes = inner.context.inner.routes.lock().unwrap().clone();
                    find_route(&page_routes, &context_routes, &e.request.url)
                };
                tokio::spawn(handle_paused(
                    Arc::downgrade(&inner),
                    session.clone(),
                    e,
                    route,
                ));
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
                let handlers = inner.console_handlers.lock().unwrap().clone();
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
                let handlers = inner.error_handlers.lock().unwrap().clone();
                for handler in handlers {
                    handler(&text);
                }
            }
            "Page.javascriptDialogOpening" => {
                let dialog = Dialog {
                    session: session.clone(),
                    kind: params["type"].as_str().unwrap_or("alert").to_owned(),
                    message: params["message"].as_str().unwrap_or_default().to_owned(),
                    default_prompt: params["defaultPrompt"].as_str().map(str::to_owned),
                };
                let handlers = inner.dialog_handlers.lock().unwrap().clone();
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
            "Page.fileChooserOpened" => {
                let Some(backend_node_id) = params["backendNodeId"].as_i64() else {
                    continue;
                };
                let chooser = FileChooser::new(
                    Page::from_inner(inner.clone()),
                    session.clone(),
                    backend_node_id,
                    params["mode"].as_str() == Some("selectMultiple"),
                );
                let handlers = inner.file_chooser_handlers.lock().unwrap().clone();
                for handler in handlers {
                    tokio::spawn(handler(chooser.clone()));
                }
                let _ = inner.events.send(PageEvent::FileChooser(chooser));
            }
            "Inspector.targetCrashed" if child.is_none() => inner.mark_closed(),
            _ => {}
        }
    }
    if child.is_none()
        && let Some(inner) = page.upgrade()
    {
        inner.mark_closed();
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

    #[test]
    fn removing_a_frame_removes_its_descendants() {
        let mut state = PageState::default();
        state.frame_mut("main");
        state.frame_mut("a").parent_id = Some("main".into());
        state.frame_mut("a1").parent_id = Some("a".into());
        state.frame_mut("b").parent_id = Some("main".into());
        state.contexts.insert("a1".into(), (7, None));
        let mut removed = state.remove_frame("a");
        removed.sort();
        assert_eq!(removed, ["a", "a1"]);
        assert!(state.frames.contains_key("b"));
        assert!(state.contexts.is_empty());
        assert_eq!(state.frame_order, ["main", "b"]);
    }
}

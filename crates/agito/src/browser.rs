//! A connected browser: the process (when agito launched it), its protocol connection, and
//! the contexts opened in it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agito_protocol::cdp::{browser as cdp_browser, target};
use tokio::task::JoinHandle;

use crate::context::{BrowserContext, ContextOptions};
use crate::error::Result;
use crate::files::Download;
use crate::launch::OutputTail;
use crate::page::{Page, PageInner};
use crate::plugin::Plugin;
use crate::transport::{CdpConnection, CdpSessionRegistry, Session, SessionRegistry};

/// A browser agito launched or connected to. Cheap to clone.
///
/// A launched browser is killed when the last handle is dropped; call [`Browser::close`] to
/// shut it down gracefully instead.
///
/// ```no_run
/// # async fn example() -> agito::Result<()> {
/// use agito::{ContextOptions, LaunchOptions};
///
/// let browser = agito::launch(LaunchOptions::default()).await?;
/// // Two contexts share nothing: not cookies, storage or routes.
/// let alice = browser.new_context(ContextOptions::default()).await?;
/// let bob = browser.new_context(ContextOptions::default()).await?;
/// let page = alice.new_page().await?;
/// browser.close().await?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct Browser {
    pub(crate) inner: Arc<BrowserInner>,
}

/// The process behind a launched browser.
pub(crate) struct BrowserProcess {
    pub child: Option<tokio::process::Child>,
    /// A temporary profile directory to remove once the browser has gone.
    pub owned_dir: Option<PathBuf>,
    pub output: OutputTail,
}

pub(crate) struct BrowserInner {
    pub connection: CdpConnection,
    pub registry: Arc<CdpSessionRegistry>,
    process: Mutex<Option<BrowserProcess>>,
    owns_process: bool,
    version: String,
    pub plugins: Vec<Arc<dyn Plugin>>,
    contexts: Mutex<Vec<Weak<crate::context::ContextInner>>>,
    pub pages_by_session: Mutex<HashMap<String, Weak<PageInner>>>,
    closed: AtomicBool,
    pump: Mutex<Option<JoinHandle<()>>>,
}

impl Browser {
    pub(crate) async fn initialize(
        connection: CdpConnection,
        process: Option<BrowserProcess>,
        plugins: Vec<Arc<dyn Plugin>>,
        owns_process: bool,
    ) -> Result<Browser> {
        let registry = Arc::new(CdpSessionRegistry::new(connection.clone()));
        let session = registry.browser_session();
        let version = session
            .send(&cdp_browser::GetVersionParams::default())
            .await?;
        let inner = Arc::new(BrowserInner {
            connection,
            registry,
            process: Mutex::new(process),
            owns_process,
            version: version.product,
            plugins,
            contexts: Mutex::new(Vec::new()),
            pages_by_session: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            pump: Mutex::new(None),
        });
        let pump = tokio::spawn(target_pump(Arc::downgrade(&inner), session.clone()));
        *inner.pump.lock().unwrap() = Some(pump);
        // Every new page target (ours or one a page opens) attaches paused, so it is set up
        // before it runs any script or loads anything.
        session
            .send(&target::SetAutoAttachParams {
                auto_attach: true,
                wait_for_debugger_on_start: true,
                flatten: Some(true),
                filter: Some(vec![target::FilterEntry {
                    exclude: None,
                    r#type: Some("page".into()),
                }]),
            })
            .await?;
        Ok(Browser { inner })
    }

    /// The browser's product and version, such as `HeadlessChrome/131.0.0.0`.
    pub fn version(&self) -> &str {
        &self.inner.version
    }

    /// Whether the connection is still open.
    pub fn is_connected(&self) -> bool {
        !self.inner.closed.load(Ordering::SeqCst) && !self.inner.connection.is_closed()
    }

    /// The last lines the browser process wrote to stdout/stderr (empty for a connected
    /// browser), for diagnosing crashes.
    pub fn process_output(&self) -> String {
        self.inner
            .process
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| p.output.describe())
            .unwrap_or_default()
    }

    /// Whether agito launched this browser (and so closes it).
    pub fn owns_process(&self) -> bool {
        self.inner.owns_process
    }

    /// Resolves when the connection to the browser is lost or closed.
    pub async fn disconnected(&self) {
        self.inner.connection.closed().await;
    }

    /// The contexts still open.
    pub fn contexts(&self) -> Vec<BrowserContext> {
        let mut contexts = self.inner.contexts.lock().unwrap();
        contexts.retain(|c| c.strong_count() > 0);
        contexts
            .iter()
            .filter_map(|c| c.upgrade())
            .map(BrowserContext::from_inner)
            .collect()
    }

    /// Opens a new, isolated context (its own cookies, storage and cache).
    pub async fn new_context(&self, options: ContextOptions) -> Result<BrowserContext> {
        let result = self
            .browser_session()
            .send(&target::CreateBrowserContextParams {
                dispose_on_detach: Some(!self.inner.owns_process),
                proxy_server: options.proxy_server.clone(),
                ..Default::default()
            })
            .await?;
        let state = options.storage_state.clone();
        let context =
            BrowserContext::create(self.clone(), result.browser_context_id, options).await?;
        self.inner
            .contexts
            .lock()
            .unwrap()
            .push(Arc::downgrade(&context.inner));
        if let Some(state) = state
            && let Err(e) = context.set_storage_state(&state).await
        {
            let _ = context.close().await;
            return Err(e);
        }
        Ok(context)
    }

    /// Opens a new context with one page in it.
    pub async fn new_page(&self, options: ContextOptions) -> Result<Page> {
        let context = self.new_context(options).await?;
        context.new_page().await
    }

    /// Closes the browser. A launched browser is shut down and its temporary profile removed;
    /// a connected one is only disconnected from.
    pub async fn close(&self) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        for context in self.contexts() {
            let _ = context.close().await;
        }
        if self.inner.owns_process && !self.inner.connection.is_closed() {
            let _ = self
                .browser_session()
                .send(&cdp_browser::CloseParams::default())
                .await;
        }
        self.inner.connection.close().await;
        let process = self.inner.process.lock().unwrap().take();
        if let Some(mut process) = process {
            if let Some(child) = process.child.as_mut()
                && tokio::time::timeout(Duration::from_secs(5), child.wait())
                    .await
                    .is_err()
            {
                let _ = child.kill().await;
            }
            if let Some(dir) = process.owned_dir.take() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
        Ok(())
    }

    /// Drops the connection but leaves the browser running (for a connected browser). For a
    /// launched browser this is the same as [`Browser::close`], since the browser cannot outlive
    /// its pipe.
    pub async fn disconnect(&self) -> Result<()> {
        if self.inner.owns_process {
            return self.close().await;
        }
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.connection.close().await;
        Ok(())
    }

    pub(crate) fn browser_session(&self) -> Arc<dyn Session> {
        self.inner.registry.browser_session()
    }

    pub(crate) fn registry(&self) -> &Arc<CdpSessionRegistry> {
        &self.inner.registry
    }

    fn context_by_id(&self, id: &str) -> Option<BrowserContext> {
        self.contexts().into_iter().find(|c| c.id() == id)
    }

    pub(crate) fn register_page(&self, session_id: &str, page: &Arc<PageInner>) {
        self.inner
            .pages_by_session
            .lock()
            .unwrap()
            .insert(session_id.to_owned(), Arc::downgrade(page));
    }
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("version", &self.inner.version)
            .field("connected", &self.is_connected())
            .finish()
    }
}

impl Drop for BrowserInner {
    fn drop(&mut self) {
        if let Some(pump) = self.pump.get_mut().unwrap().take() {
            pump.abort();
        }
        let Some(mut process) = self.process.get_mut().unwrap().take() else {
            return;
        };
        if let Some(child) = process.child.as_mut() {
            let _ = child.start_kill();
            // Give the process a moment to exit so its profile directory can be removed.
            for _ in 0..40 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if let Some(dir) = process.owned_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Watches browser-level target events: marks pages closed when their target detaches.
async fn target_pump(browser: Weak<BrowserInner>, session: Arc<dyn Session>) {
    let mut events = session.subscribe(&[
        "Target.detachedFromTarget",
        "Target.attachedToTarget",
        "Browser.downloadWillBegin",
        "Browser.downloadProgress",
    ]);
    while let Some(event) = events.next().await {
        let Some(inner) = browser.upgrade() else {
            return;
        };
        match event.method.as_str() {
            "Target.attachedToTarget" => {
                let Ok(e) = serde_json::from_value::<target::AttachedToTargetEvent>(event.params)
                else {
                    continue;
                };
                let browser = Browser { inner };
                let context = e
                    .target_info
                    .browser_context_id
                    .as_deref()
                    .filter(|_| e.target_info.r#type == "page")
                    .and_then(|id| browser.context_by_id(id));
                match context {
                    Some(context) => {
                        tokio::spawn(async move {
                            context
                                .adopt_target(e.session_id, e.target_info, e.waiting_for_debugger)
                                .await;
                        });
                    }
                    // Not ours (another context, or not a page): let it run and leave it be.
                    None => {
                        tokio::spawn(release_target(browser, e.session_id));
                    }
                }
            }
            "Browser.downloadWillBegin" => {
                let Ok(e) =
                    serde_json::from_value::<cdp_browser::DownloadWillBeginEvent>(event.params)
                else {
                    continue;
                };
                let browser = Browser { inner };
                // The download belongs to the page whose frame started it.
                let owner = browser.contexts().into_iter().find_map(|context| {
                    context
                        .pages()
                        .into_iter()
                        .find(|p| p.frame_by_id(&e.frame_id).is_some())
                        .map(|p| (context, p))
                });
                let Some((context, page)) = owner else {
                    continue;
                };
                let download = Download::new(
                    &e,
                    &context.inner.download_dir,
                    context.id().to_owned(),
                    Arc::downgrade(&page.inner),
                    browser.browser_session(),
                );
                context
                    .inner
                    .downloads
                    .lock()
                    .unwrap()
                    .insert(download.guid().to_owned(), download.clone());
                page.inner.notify_download(&download);
            }
            "Browser.downloadProgress" => {
                let Ok(e) =
                    serde_json::from_value::<cdp_browser::DownloadProgressEvent>(event.params)
                else {
                    continue;
                };
                let browser = Browser { inner };
                for context in browser.contexts() {
                    let download = context
                        .inner
                        .downloads
                        .lock()
                        .unwrap()
                        .get(&e.guid)
                        .cloned();
                    if let Some(download) = download {
                        download.on_progress(&e);
                        break;
                    }
                }
            }
            _ => {
                if let Some(session_id) = event.params["sessionId"].as_str() {
                    let page = inner.pages_by_session.lock().unwrap().remove(session_id);
                    if let Some(page) = page.and_then(|p| p.upgrade()) {
                        page.mark_closed();
                    }
                    inner.registry.remove_session(session_id);
                }
            }
        }
    }
    // The connection has gone: every page with it.
    if let Some(browser) = browser.upgrade() {
        let pages: Vec<_> = browser.pages_by_session.lock().unwrap().drain().collect();
        for (_, page) in pages {
            if let Some(page) = page.upgrade() {
                page.mark_closed();
            }
        }
    }
}

/// Resumes a target agito attached to but does not drive, and detaches from it.
async fn release_target(browser: Browser, session_id: String) {
    let registry = browser.registry().clone();
    let session = registry.create_session(&session_id);
    let _ = session
        .send(&agito_protocol::cdp::runtime::RunIfWaitingForDebuggerParams {})
        .await;
    let _ = browser
        .browser_session()
        .send(&target::DetachFromTargetParams {
            session_id: Some(session_id.clone()),
            ..Default::default()
        })
        .await;
    registry.remove_session(&session_id);
}

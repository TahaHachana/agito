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
use crate::launch::OutputTail;
use crate::page::{Page, PageInner};
use crate::plugin::Plugin;
use crate::transport::{CdpConnection, CdpSessionRegistry, Session, SessionRegistry};

/// A browser agito launched or connected to. Cheap to clone.
///
/// A launched browser is killed when the last handle is dropped; call [`Browser::close`] to
/// shut it down gracefully instead.
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
        let pump = tokio::spawn(target_pump(Arc::downgrade(&inner), session));
        *inner.pump.lock().unwrap() = Some(pump);
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
        let context =
            BrowserContext::create(self.clone(), result.browser_context_id, options).await?;
        self.inner
            .contexts
            .lock()
            .unwrap()
            .push(Arc::downgrade(&context.inner));
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
    let mut events = session.subscribe(&["Target.detachedFromTarget"]);
    while let Some(event) = events.next().await {
        let Some(browser) = browser.upgrade() else {
            return;
        };
        if let Some(session_id) = event.params["sessionId"].as_str() {
            let page = browser.pages_by_session.lock().unwrap().remove(session_id);
            if let Some(page) = page.and_then(|p| p.upgrade()) {
                page.mark_closed();
            }
            browser.registry.remove_session(session_id);
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

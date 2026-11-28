//! What one MCP session holds: a browser started on first use, its contexts and tabs, which
//! of them is active, and the latest snapshot of each page.

use std::collections::HashMap;
use std::sync::Arc;

use agito::{Browser, BrowserContext, Frame, Locator, Page};
use tokio::sync::Mutex;

use crate::ServerOptions;
use crate::snapshot::{self, InlineFrames, RefTarget};

/// The context a session uses until it names another.
pub const DEFAULT_CONTEXT: &str = "default";

/// Why a target could not be turned into a locator.
#[derive(Debug)]
pub(crate) enum TargetError {
    /// A ref, before any snapshot of the page.
    NoSnapshot,
    /// A ref the latest snapshot did not hand out.
    Stale(String),
}

impl TargetError {
    /// The message an agent reads: what went wrong and the way out.
    pub fn message(&self) -> String {
        match self {
            TargetError::NoSnapshot => {
                "No snapshot has been taken. Call snapshot first, then retry \
                with a ref from it, or pass a selector instead."
                    .to_owned()
            }
            TargetError::Stale(r) => format!(
                "Ref '{r}' is not in the latest snapshot. Call snapshot to refresh refs, then \
                 retry, or pass a selector instead."
            ),
        }
    }
}

/// A page's latest snapshot: its refs, and the document they were read from.
#[derive(Default)]
pub(crate) struct PageSnapshot {
    refs: Option<HashMap<String, RefTarget>>,
    /// The frame the snapshot described, or `None` for the page.
    scope: Option<Frame>,
}

impl PageSnapshot {
    /// Takes a fresh snapshot of `page`, or of the frame `scope`, rooted at `root_ref` from the
    /// previous snapshot when given, and replaces the refs.
    pub async fn take(
        &mut self,
        page: &Page,
        scope: Option<Frame>,
        root_ref: Option<&str>,
        max_depth: Option<usize>,
    ) -> Result<(String, Option<Frame>), SnapshotError> {
        let root = match root_ref {
            None => None,
            Some(r) => {
                let refs = self
                    .refs
                    .as_ref()
                    .ok_or(SnapshotError::Target(TargetError::NoSnapshot))?;
                Some(
                    refs.get(r)
                        .cloned()
                        .ok_or_else(|| SnapshotError::Target(TargetError::Stale(r.to_owned())))?,
                )
            }
        };
        // A ref inside a frame roots the snapshot in that frame's document, which scopes it.
        // An unprefixed ref belongs to the document the previous snapshot covered.
        let scope = match &root {
            Some(target) => target.frame.clone().or_else(|| self.scope.clone()),
            None => scope,
        };
        let tree = match &scope {
            Some(frame) => frame.accessibility_snapshot().await,
            None => page.accessibility_snapshot().await,
        }
        .map_err(SnapshotError::Browser)?;

        let roots = match &root {
            Some(target) => vec![
                snapshot::find_by_backend_id(&tree.roots, target.backend_node_id)
                    .cloned()
                    .ok_or_else(|| {
                        SnapshotError::Target(TargetError::Stale(root_ref.unwrap().to_owned()))
                    })?,
            ],
            None => tree.roots,
        };
        // Only a page snapshot reaches into frames; collecting them comes with frame support.
        let frames: Option<InlineFrames> = None;
        let serialized = snapshot::serialize(&roots, max_depth, frames.as_ref());
        let mut text = serialized.text;
        if root_ref.is_none() && serialized.refs.is_empty() {
            text = format!(
                "{}\n\nNote: no addressable elements were found. The page may render to a canvas \
                 or custom surface that the accessibility tree cannot describe. Take a screenshot \
                 to identify controls visually.\n",
                text.trim_end_matches('\n')
            );
        }
        self.refs = Some(serialized.refs);
        self.scope = scope.clone();
        Ok((text, scope))
    }

    /// A locator for `target`: a ref from the latest snapshot, or a selector, searched in
    /// `selector_scope` (the selected frame) or the page. A ref keeps addressing the document
    /// it was read from, whatever is selected now.
    pub fn resolve(
        &self,
        page: &Page,
        target: &str,
        selector_scope: Option<&Frame>,
    ) -> Result<Locator, TargetError> {
        if !snapshot::is_ref_shaped(target) {
            return Ok(match selector_scope {
                Some(frame) => frame.locator(target),
                None => page.locator(target),
            });
        }
        let refs = self.refs.as_ref().ok_or(TargetError::NoSnapshot)?;
        let found = refs
            .get(target)
            .ok_or_else(|| TargetError::Stale(target.to_owned()))?;
        Ok(match found.frame.as_ref().or(self.scope.as_ref()) {
            Some(frame) => frame.locator_by_backend_node_id(found.backend_node_id),
            None => page.locator_by_backend_node_id(found.backend_node_id),
        })
    }
}

/// Why a snapshot could not be taken.
#[derive(Debug)]
pub(crate) enum SnapshotError {
    Target(TargetError),
    Browser(agito::Error),
}

/// One named context, its tabs, and the active one.
struct HeldContext {
    context: BrowserContext,
    /// Every page the context opened, by `new_page` or by a page (popups). A context holds
    /// its pages weakly, so a page lives only while something holds it: between tool calls,
    /// that is this list.
    pages: Arc<std::sync::Mutex<Vec<Page>>>,
    /// The active tab's target id.
    active_page: Option<String>,
}

impl HeldContext {
    fn new(context: BrowserContext) -> Self {
        let pages: Arc<std::sync::Mutex<Vec<Page>>> = Arc::default();
        let keep = pages.clone();
        context.on_page(move |page| keep.lock().unwrap().push(page.clone()));
        HeldContext {
            context,
            pages,
            active_page: None,
        }
    }

    /// The open tabs, in the order they opened.
    fn pages(&self) -> Vec<Page> {
        let mut pages = self.pages.lock().unwrap();
        pages.retain(|p| !p.is_closed());
        pages.clone()
    }
}

/// The browser one MCP session drives.
pub(crate) struct Session {
    options: Arc<ServerOptions>,
    browser: Option<Browser>,
    contexts: Vec<(String, HeldContext)>,
    active_context: String,
    /// Each page's latest snapshot, by target id.
    snapshots: HashMap<String, Arc<Mutex<PageSnapshot>>>,
}

impl Session {
    pub fn new(options: Arc<ServerOptions>) -> Self {
        Session {
            options,
            browser: None,
            contexts: Vec::new(),
            active_context: DEFAULT_CONTEXT.to_owned(),
            snapshots: HashMap::new(),
        }
    }

    pub fn is_browser_launched(&self) -> bool {
        self.browser.is_some()
    }

    pub fn context_names(&self) -> Vec<String> {
        self.contexts.iter().map(|(n, _)| n.clone()).collect()
    }

    pub fn active_context_name(&self) -> &str {
        &self.active_context
    }

    /// The browser, started if this is the first call that needs one, or again if the last
    /// one went away: the call that raced a crash fails, and the next one recovers.
    async fn browser(&mut self) -> agito::Result<Browser> {
        if let Some(browser) = &self.browser {
            if browser.is_connected() {
                return Ok(browser.clone());
            }
            eprintln!("agito mcp: the browser went away; starting another");
            self.browser = None;
            self.contexts.clear();
            self.snapshots.clear();
            self.active_context = DEFAULT_CONTEXT.to_owned();
        }
        let browser = agito::launch(self.options.launch_options()).await?;
        self.browser = Some(browser.clone());
        Ok(browser)
    }

    async fn active_context(&mut self) -> agito::Result<BrowserContext> {
        let browser = self.browser().await?;
        if let Some((_, held)) = self
            .contexts
            .iter()
            .find(|(n, _)| *n == self.active_context)
        {
            return Ok(held.context.clone());
        }
        let context = browser.new_context(self.options.context_options()).await?;
        self.contexts.push((
            self.active_context.clone(),
            HeldContext::new(context.clone()),
        ));
        Ok(context)
    }

    fn held(&mut self) -> Option<&mut HeldContext> {
        let name = &self.active_context;
        self.contexts
            .iter_mut()
            .find(|(n, _)| n == name)
            .map(|(_, h)| h)
    }

    /// The active tab, opened if the context has none.
    pub async fn active_page(&mut self) -> agito::Result<Page> {
        let context = self.active_context().await?;
        let held = self.held().expect("the active context was just made");
        let pages = held.pages();
        let page = held
            .active_page
            .as_ref()
            .and_then(|id| pages.iter().find(|p| p.target_id() == id && !p.is_closed()))
            .cloned()
            .or_else(|| pages.into_iter().rev().find(|p| !p.is_closed()));
        let page = match page {
            Some(page) => page,
            None => context.new_page().await?,
        };
        self.held().expect("still there").active_page = Some(page.target_id().to_owned());
        Ok(page)
    }

    /// The snapshot state of `page`.
    pub fn snapshot_of(&mut self, page: &Page) -> Arc<Mutex<PageSnapshot>> {
        self.snapshots
            .entry(page.target_id().to_owned())
            .or_default()
            .clone()
    }

    /// Forgets a page's refs, after it navigated.
    pub fn invalidate(&mut self, page: &Page) {
        self.snapshots.remove(page.target_id());
    }

    /// Every open tab across the session's contexts, in one sequence, with its context's name.
    pub fn tabs(&self) -> Vec<(String, Page)> {
        self.contexts
            .iter()
            .flat_map(|(name, held)| held.pages().into_iter().map(move |p| (name.clone(), p)))
            .collect()
    }

    pub async fn open_tab(&mut self) -> agito::Result<Page> {
        let context = self.active_context().await?;
        let page = context.new_page().await?;
        self.held().expect("the active context exists").active_page =
            Some(page.target_id().to_owned());
        Ok(page)
    }

    /// Makes tab `index` active, switching to its context.
    pub fn select_tab(&mut self, index: usize) -> Result<Page, String> {
        let tabs = self.tabs();
        let Some((context, page)) = tabs.get(index).cloned() else {
            return Err(no_such_tab(index, tabs.len()));
        };
        self.active_context = context;
        self.held().expect("the tab's context is held").active_page =
            Some(page.target_id().to_owned());
        Ok(page)
    }

    /// Closes tab `index`, or the active tab; returns the index closed.
    pub async fn close_tab(&mut self, index: Option<usize>) -> Result<usize, String> {
        let tabs = self.tabs();
        let index = match index {
            Some(i) => i,
            None => {
                let active = self
                    .contexts
                    .iter()
                    .find(|(n, _)| *n == self.active_context)
                    .and_then(|(_, h)| h.active_page.clone());
                tabs.iter()
                    .position(|(c, p)| {
                        *c == self.active_context && Some(p.target_id()) == active.as_deref()
                    })
                    .ok_or_else(|| "No tab is open.".to_owned())?
            }
        };
        let Some((_, page)) = tabs.get(index) else {
            return Err(no_such_tab(index, tabs.len()));
        };
        self.snapshots.remove(page.target_id());
        page.close().await.map_err(|e| e.to_string())?;
        Ok(index)
    }

    /// Closes the browser, if this session started one.
    pub async fn shutdown(&mut self) {
        if let Some(browser) = self.browser.take() {
            let _ = browser.close().await;
        }
    }
}

fn no_such_tab(index: usize, open: usize) -> String {
    match open {
        0 => format!("There is no tab {index}: no tabs are open."),
        n => format!(
            "There is no tab {index}: {n} tab{} open, numbered 0 to {}. Call tab_list to see them.",
            if n == 1 { " is" } else { "s are" },
            n - 1
        ),
    }
}

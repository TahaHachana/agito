//! What one MCP session holds: a browser started on first use, its contexts and tabs, which
//! of them is active, and the latest snapshot of each page.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agito::accessibility::AxNode;
use agito::{Browser, BrowserContext, Frame, Locator, Page};

use crate::ServerOptions;
use crate::snapshot::{self, FrameTree, InlineFrames, RefTarget};
use crate::tab::Tab;

/// How many frames a page snapshot prints inside itself unless asked otherwise.
pub const DEFAULT_MAX_FRAMES: usize = 10;

/// The context a session uses until it names another.
pub const DEFAULT_CONTEXT: &str = "default";

/// Why a target could not be turned into a locator.
#[derive(Debug)]
pub(crate) enum TargetError {
    /// A ref, before any snapshot of the page.
    NoSnapshot,
    /// A ref, after the page navigated away from the snapshot that handed it out.
    Retired,
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
            TargetError::Retired => "The page navigated since the last snapshot, so its refs \
                no longer address anything. Call snapshot for new refs, or pass a selector \
                instead."
                .to_owned(),
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
    /// The scope's URL when it was read, to notice the frame navigating under the refs.
    scope_url: String,
    /// The frames printed inside a page snapshot, with each one's index and URL when read.
    frames: Vec<(usize, Frame, String)>,
    /// The roots of the document read, for describing nodes that took no ref.
    roots: Vec<AxNode>,
    /// Whether the refs were retired by a navigation, rather than never taken.
    retired: bool,
}

/// Every frame of `page`, in the order frame tools number them: the page first, each frame
/// after the one holding it, siblings in the order they attached. With each frame's depth.
pub(crate) fn frame_order(page: &Page) -> Vec<(Frame, usize)> {
    fn walk(frame: Frame, depth: usize, out: &mut Vec<(Frame, usize)>) {
        let children = frame.child_frames();
        out.push((frame, depth));
        for child in children {
            walk(child, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(page.main_frame(), 0, &mut out);
    out
}

/// The frame documents a page snapshot prints, up to `max_frames`, found by the element
/// hosting each: a frame whose host element is not in the tree (a hidden iframe, say) is left
/// out. Hosts are matched exactly, through the browser's record of each frame's owner element.
async fn collect_frames(page: &Page, roots: &[AxNode], max_frames: usize) -> InlineFrames {
    let mut collected = InlineFrames::default();
    let order = frame_order(page);
    if max_frames == 0 || order.len() <= 1 {
        return collected;
    }
    let index_of = |frame: &Frame| order.iter().position(|(f, _)| f == frame);
    // Documents still to look inside: the host frame (`None` for the page) and its tree.
    let mut queue: Vec<(Option<Frame>, Frame, Vec<AxNode>)> =
        vec![(None, page.main_frame(), roots.to_vec())];
    while let Some((host, document, tree)) = queue.pop() {
        let hosts: HashSet<i64> = tree
            .iter()
            .flat_map(AxNode::walk)
            .filter(|n| n.role.eq_ignore_ascii_case("Iframe"))
            .filter_map(|n| n.backend_node_id)
            .collect();
        if hosts.is_empty() {
            continue;
        }
        let mut found = Vec::new();
        for child in document.child_frames() {
            if collected.len() >= max_frames {
                break;
            }
            let Ok(Some(owner)) = child.owner_backend_node_id().await else {
                continue;
            };
            if !hosts.contains(&owner) {
                continue;
            }
            let (Some(index), Ok(inner)) = (index_of(&child), child.accessibility_snapshot().await)
            else {
                continue;
            };
            collected.add(
                host.as_ref(),
                owner,
                FrameTree {
                    index,
                    frame: child.clone(),
                    roots: inner.roots.clone(),
                },
            );
            found.push((Some(child.clone()), child, inner.roots));
        }
        // Depth first, in document order.
        queue.extend(found.into_iter().rev());
    }
    collected
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
        max_frames: Option<usize>,
    ) -> Result<(String, Option<Frame>), SnapshotError> {
        let root = match root_ref {
            None => None,
            Some(r) => {
                let refs = self
                    .refs
                    .as_ref()
                    .ok_or(SnapshotError::Target(self.missing()))?;
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
        // Only a page snapshot reaches into frames. Inside one frame the caller has said which
        // document it is looking at, and prefixed refs there would name frames by an index the
        // printout never showed.
        let frames = match scope {
            None => {
                Some(collect_frames(page, &roots, max_frames.unwrap_or(DEFAULT_MAX_FRAMES)).await)
            }
            Some(_) => None,
        };
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
        // A frame left out of the tree is one the agent cannot see at all: say how many.
        let missing = page
            .frames()
            .len()
            .saturating_sub(1 + serialized.frames.len());
        if scope.is_none() && missing > 0 {
            text = format!(
                "{}\n\nNote: this page has {missing} more frame{} whose contents are not in this \
                 tree. Use frame_list to see them and frame_select to look inside one.\n",
                text.trim_end_matches('\n'),
                if missing == 1 { "" } else { "s" }
            );
        }
        self.refs = Some(serialized.refs);
        self.scope_url = scope.as_ref().map(Frame::url).unwrap_or_default();
        self.scope = scope.clone();
        self.frames = serialized
            .frames
            .into_iter()
            .map(|t| {
                let url = t.frame.url();
                (t.index, t.frame, url)
            })
            .collect();
        self.roots = roots;
        self.retired = false;
        Ok((text, scope))
    }

    /// Forgets the refs, because the page navigated away from the document they named.
    pub fn retire(&mut self) {
        if self.refs.is_some() || self.retired {
            *self = PageSnapshot {
                retired: true,
                ..PageSnapshot::default()
            };
        }
    }

    fn missing(&self) -> TargetError {
        if self.retired {
            TargetError::Retired
        } else {
            TargetError::NoSnapshot
        }
    }

    /// Whether a snapshot has been taken since the page last navigated.
    pub fn taken(&self) -> bool {
        self.refs.is_some()
    }

    /// The frames whose refs a navigation retired: each printed frame that has gone or moved
    /// since it was read.
    pub fn moved_frames(&self) -> Vec<usize> {
        self.frames
            .iter()
            .filter(|(_, f, url)| f.is_detached() || f.url() != *url)
            .map(|(i, _, _)| *i)
            .collect()
    }

    /// Whether the frame a scoped snapshot described has gone or moved since.
    pub fn scope_moved(&self) -> bool {
        self.scope
            .as_ref()
            .is_some_and(|f| f.is_detached() || f.url() != self.scope_url)
    }

    /// The ref the snapshot gave the node `backend_node_id` in the document at `frame_url`,
    /// where `page_url` is the snapshot's own document.
    pub fn ref_for(&self, frame_url: &str, page_url: &str, backend_node_id: i64) -> Option<String> {
        let refs = self.refs.as_ref()?;
        refs.iter()
            .filter(|(_, t)| t.backend_node_id == backend_node_id)
            .find(|(_, t)| match &t.frame {
                Some(frame) => frame.url() == frame_url,
                None => frame_url == page_url,
            })
            .map(|(id, _)| id.clone())
    }

    /// The text the snapshot holds under a node of its own document, as it would print it.
    pub fn text_for(&self, backend_node_id: i64) -> Option<String> {
        snapshot::find_by_backend_id(&self.roots, backend_node_id).and_then(snapshot::inline_text)
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
        let refs = self.refs.as_ref().ok_or_else(|| self.missing())?;
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
    /// Every page the context opened, by `new_page` or by a page (popups), with the state
    /// kept for it. A context holds its pages weakly, so a page lives only while something
    /// holds it: between tool calls, that is this list.
    tabs: Arc<std::sync::Mutex<Vec<Tab>>>,
    /// The active tab's target id.
    active_page: Option<String>,
}

impl HeldContext {
    fn new(context: BrowserContext, options: &ServerOptions) -> Self {
        let tabs: Arc<std::sync::Mutex<Vec<Tab>>> = Arc::default();
        let keep = tabs.clone();
        let policy = options.dialogs;
        context.on_page(move |page| keep.lock().unwrap().push(Tab::attach(page, policy)));
        HeldContext {
            context,
            tabs,
            active_page: None,
        }
    }

    /// The open tabs, in the order they opened.
    fn tabs(&self) -> Vec<Tab> {
        let mut tabs = self.tabs.lock().unwrap();
        tabs.retain(|t| !t.page.is_closed());
        tabs.clone()
    }
}

/// The browser one MCP session drives.
pub(crate) struct Session {
    options: Arc<ServerOptions>,
    browser: Option<Browser>,
    contexts: Vec<(String, HeldContext)>,
    active_context: String,
}

impl Session {
    pub fn new(options: Arc<ServerOptions>) -> Self {
        Session {
            options,
            browser: None,
            contexts: Vec::new(),
            active_context: DEFAULT_CONTEXT.to_owned(),
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
            HeldContext::new(context.clone(), &self.options),
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
    pub async fn active_tab(&mut self) -> agito::Result<Tab> {
        let context = self.active_context().await?;
        let held = self.held().expect("the active context was just made");
        let tabs = held.tabs();
        let tab = held
            .active_page
            .as_ref()
            .and_then(|id| tabs.iter().find(|t| t.page.target_id() == id))
            .cloned()
            .or_else(|| tabs.last().cloned());
        let tab = match tab {
            Some(tab) => tab,
            None => {
                let page = context.new_page().await?;
                self.tab_of(&page)
            }
        };
        self.held().expect("still there").active_page = Some(tab.page.target_id().to_owned());
        Ok(tab)
    }

    /// The active tab, if the session has one, without starting a browser or opening a tab.
    pub fn current_tab(&self) -> Option<Tab> {
        let (_, held) = self
            .contexts
            .iter()
            .find(|(n, _)| *n == self.active_context)?;
        let id = held.active_page.as_deref()?;
        held.tabs().into_iter().find(|t| t.page.target_id() == id)
    }

    /// The tab for a page this session's contexts opened.
    fn tab_of(&self, page: &Page) -> Tab {
        self.tabs()
            .into_iter()
            .map(|(_, tab)| tab)
            .find(|t| t.page.target_id() == page.target_id())
            .expect("on_page records every page a held context opens")
    }

    /// Every open tab across the session's contexts, in one sequence, with its context's name.
    pub fn tabs(&self) -> Vec<(String, Tab)> {
        self.contexts
            .iter()
            .flat_map(|(name, held)| held.tabs().into_iter().map(move |t| (name.clone(), t)))
            .collect()
    }

    pub async fn open_tab(&mut self) -> agito::Result<Tab> {
        let context = self.active_context().await?;
        let page = context.new_page().await?;
        self.held().expect("the active context exists").active_page =
            Some(page.target_id().to_owned());
        Ok(self.tab_of(&page))
    }

    /// Makes tab `index` active, switching to its context.
    pub fn select_tab(&mut self, index: usize) -> Result<Tab, String> {
        let tabs = self.tabs();
        let Some((context, tab)) = tabs.get(index).cloned() else {
            return Err(no_such_tab(index, tabs.len()));
        };
        self.active_context = context;
        self.held().expect("the tab's context is held").active_page =
            Some(tab.page.target_id().to_owned());
        // A frame selected before belongs to a document the agent has moved away from.
        tab.state.select_frame(None);
        Ok(tab)
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
                    .position(|(c, t)| {
                        *c == self.active_context && Some(t.page.target_id()) == active.as_deref()
                    })
                    .ok_or_else(|| "No tab is open.".to_owned())?
            }
        };
        let Some((_, tab)) = tabs.get(index) else {
            return Err(no_such_tab(index, tabs.len()));
        };
        tab.page.close().await.map_err(|e| e.to_string())?;
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

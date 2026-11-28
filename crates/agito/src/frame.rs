//! Frames: the main document and every `<iframe>` in it, same-process or out-of-process.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agito_protocol::cdp::{dom, page as cdp_page, runtime};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, ErrorKind, Result};
use crate::handles::{ElementHandle, call_function, decode_value, evaluation_error, to_arg};
use crate::locator::Locator;
use crate::page::{NavigateOptions, Page, PageEvent, Response, UrlMatcher, WaitUntil};
use crate::transport::Session;

/// How long to wait for a frame's script context before giving up.
const CONTEXT_TIMEOUT: Duration = Duration::from_secs(30);

/// Adds the `get_by_*` locator builders to a type with a `locator(&str) -> Locator` method.
macro_rules! impl_get_by {
    ($ty:ty) => {
        impl $ty {
            /// Elements with an ARIA role and, optionally, accessible name.
            pub fn get_by_role(&self, role: &str, name: Option<&str>) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::role_selector(role, name))
            }

            /// Elements whose text contains `text`, ignoring case.
            pub fn get_by_text(&self, text: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::get_by_text(text, false))
            }

            /// Elements whose whole text equals `text`.
            pub fn get_by_text_exact(&self, text: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::get_by_text(text, true))
            }

            /// Form controls by their label text.
            pub fn get_by_label(&self, text: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::get_by_label(text, false))
            }

            /// Inputs by placeholder text.
            pub fn get_by_placeholder(&self, text: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::get_by_attribute(
                    "placeholder",
                    text,
                    false,
                ))
            }

            /// Elements by `title` attribute.
            pub fn get_by_title(&self, text: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::get_by_attribute("title", text, false))
            }

            /// Images by alt text.
            pub fn get_by_alt_text(&self, text: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::get_by_attribute("alt", text, false))
            }

            /// Elements by `data-testid`.
            pub fn get_by_test_id(&self, test_id: &str) -> $crate::locator::Locator {
                self.locator(&$crate::selectors::test_id_selector(test_id))
            }
        }
    };
}
pub(crate) use impl_get_by;

/// A frame in a page. Cheap to clone; it names the frame, and reads the frame's current state
/// from the page each time.
#[derive(Clone)]
pub struct Frame {
    page: Page,
    id: String,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("id", &self.id)
            .field("url", &self.url())
            .finish()
    }
}

impl PartialEq for Frame {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && Arc::ptr_eq(&self.page.inner, &other.page.inner)
    }
}

impl Frame {
    pub(crate) fn new(page: Page, id: String) -> Self {
        Self { page, id }
    }

    /// The page this frame belongs to.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The protocol's id for this frame.
    pub fn id(&self) -> &str {
        &self.id
    }

    fn data<T>(&self, read: impl FnOnce(&crate::page::FrameData) -> T) -> Option<T> {
        self.page
            .inner
            .state
            .lock()
            .unwrap()
            .frames
            .get(&self.id)
            .map(read)
    }

    /// The frame's `name` attribute (empty when it has none).
    pub fn name(&self) -> String {
        self.data(|f| f.name.clone()).unwrap_or_default()
    }

    /// The frame's current URL.
    pub fn url(&self) -> String {
        self.data(|f| f.url.clone()).unwrap_or_default()
    }

    /// Whether this is the page's top-level frame.
    pub fn is_main(&self) -> bool {
        self.page.inner.state.lock().unwrap().main_frame_id == self.id
    }

    /// Whether the frame runs in its own renderer process (a cross-site iframe under site
    /// isolation), driven over a session of its own.
    pub fn is_out_of_process(&self) -> bool {
        self.data(|f| f.session_id.is_some()).unwrap_or(false)
    }

    /// Whether the frame has been removed from the page.
    pub fn is_detached(&self) -> bool {
        self.data(|_| ()).is_none()
    }

    /// The frame containing this one; `None` for the main frame.
    pub fn parent_frame(&self) -> Option<Frame> {
        let parent = self.data(|f| f.parent_id.clone()).flatten()?;
        Some(Frame::new(self.page.clone(), parent))
    }

    /// The frames directly inside this one.
    pub fn child_frames(&self) -> Vec<Frame> {
        self.page
            .frames()
            .into_iter()
            .filter(|f| f.data(|d| d.parent_id.as_deref() == Some(self.id.as_str())) == Some(true))
            .collect()
    }

    /// The session that owns this frame: the page's own, or an out-of-process iframe's.
    pub(crate) fn session(&self) -> Arc<dyn Session> {
        self.page.inner.session_for(&self.id)
    }

    fn detached_error(&self) -> Error {
        ErrorKind::Evaluation {
            message: format!("Frame {} was detached from the page.", self.id),
        }
        .into()
    }

    /// The frame's default execution context, waiting for it to exist. The main frame falls
    /// back to the session's default context, which is the same thing.
    async fn context_id(&self, timeout: Duration) -> Result<Option<i64>> {
        let mut events = self.page.inner.events.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            {
                let state = self.page.inner.state.lock().unwrap();
                if !state.frames.contains_key(&self.id) {
                    return Err(self.detached_error());
                }
                if let Some((id, _)) = state.contexts.get(&self.id) {
                    return Ok(Some(*id));
                }
                if state.main_frame_id == self.id {
                    return Ok(None);
                }
            }
            self.page.ensure_open()?;
            match self.page.next_event(&mut events, deadline).await {
                Some(PageEvent::Closed) => return Err(self.page.closed_error()),
                Some(PageEvent::FrameDetached { frame_id }) if frame_id == self.id => {
                    return Err(self.detached_error());
                }
                // Anything else, including this frame's context arriving: look again.
                Some(PageEvent::ContextCreated) | Some(_) => {}
                None => {
                    return Err(ErrorKind::WaitTimeout {
                        condition: "frame execution context".into(),
                        timeout,
                        last_evaluated_value: None,
                        message: format!(
                            "Frame {} ({}) had no script context after {}ms.",
                            self.id,
                            self.url(),
                            timeout.as_millis()
                        ),
                    }
                    .into());
                }
            }
        }
    }

    /// `Runtime.evaluate` in this frame's main world. A context that went stale under the
    /// call (the frame navigated) is looked up again once.
    pub(crate) async fn evaluate_raw(
        &self,
        expression: &str,
        return_by_value: bool,
    ) -> Result<runtime::RemoteObject> {
        self.page.ensure_open()?;
        for attempt in 0..2 {
            let context_id = self.context_id(CONTEXT_TIMEOUT).await?;
            let result = self
                .session()
                .send(&runtime::EvaluateParams {
                    expression: expression.to_owned(),
                    return_by_value: Some(return_by_value),
                    await_promise: Some(true),
                    context_id,
                    ..Default::default()
                })
                .await;
            match result {
                Ok(r) => {
                    if let Some(details) = r.exception_details {
                        return Err(evaluation_error(&details));
                    }
                    return Ok(r.result);
                }
                Err(e)
                    if attempt == 0
                        && context_id.is_some()
                        && e.to_string().contains("Cannot find context") =>
                {
                    self.forget_context(context_id);
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("the second attempt always returns")
    }

    fn forget_context(&self, context_id: Option<i64>) {
        let mut state = self.page.inner.state.lock().unwrap();
        if state.contexts.get(&self.id).map(|(id, _)| Some(*id)) == Some(context_id) {
            state.contexts.remove(&self.id);
        }
    }

    /// Evaluates an expression returning an array of elements and splits it into handles.
    pub async fn query_all(&self, expression: &str) -> Result<Vec<ElementHandle>> {
        let array = self.evaluate_raw(expression, false).await?;
        crate::handles::array_to_handles(&self.session(), array).await
    }

    /// Evaluates a JavaScript expression in this frame and decodes its value. A function
    /// expression (`() => document.title`) is called; a promise is awaited.
    pub async fn evaluate<T: DeserializeOwned>(&self, expression: &str) -> Result<T> {
        let wrapped = format!(
            "(async () => {{ const __v = ({expression}); return typeof __v === 'function' ? await __v() : await __v; }})()"
        );
        decode_value(self.evaluate_raw(&wrapped, !discards::<T>()).await?)
    }

    /// Calls a JavaScript function in this frame with one serializable argument.
    pub async fn evaluate_with<T: DeserializeOwned, A: Serialize>(
        &self,
        function: &str,
        arg: &A,
    ) -> Result<T> {
        let arg = to_arg(arg)?;
        let expression = format!("(async () => await ({function})({arg}))()");
        decode_value(self.evaluate_raw(&expression, !discards::<T>()).await?)
    }

    /// The frame document's title.
    pub async fn title(&self) -> Result<String> {
        self.evaluate("document.title").await
    }

    /// The frame document's HTML, doctype included.
    pub async fn content(&self) -> Result<String> {
        self.evaluate(
            "(() => { let s = ''; if (document.doctype) s = new XMLSerializer().serializeToString(document.doctype); \
             if (document.documentElement) s += document.documentElement.outerHTML; return s; })",
        )
        .await
    }

    /// Replaces the frame's document with `html`.
    pub async fn set_content(&self, html: &str) -> Result<()> {
        self.page.ensure_open()?;
        self.session()
            .send(&cdp_page::SetDocumentContentParams {
                frame_id: self.id.clone(),
                html: html.to_owned(),
            })
            .await?;
        Ok(())
    }

    /// Navigates this frame and waits for `load`. Returns the document's response, if any.
    pub async fn goto(&self, url: &str) -> Result<Option<Response>> {
        self.goto_with(url, NavigateOptions::default()).await
    }

    /// [`Frame::goto`] with options.
    pub async fn goto_with(&self, url: &str, options: NavigateOptions) -> Result<Option<Response>> {
        self.page.ensure_open()?;
        if self.is_detached() {
            return Err(self.detached_error());
        }
        let url = self.page.resolve_url(url);
        self.page.fire_hooks_before_navigation(&url).await;
        let timeout = self.page.navigation_timeout(&options);
        let mut events = self.page.inner.events.subscribe();
        let result = self
            .session()
            .send(&cdp_page::NavigateParams {
                url: url.clone(),
                referrer: options.referer.clone(),
                frame_id: Some(self.id.clone()),
                ..Default::default()
            })
            .await?;
        if let Some(error) = result.error_text.filter(|e| !e.is_empty()) {
            return Err(ErrorKind::Navigation {
                url: url.clone(),
                error_code: error.clone(),
                page_url: self.page.url(),
                message: format!("Navigation to {url} failed: {error}"),
            }
            .into());
        }
        let response = match result.loader_id {
            // Same-document navigation: nothing more will load.
            None => None,
            Some(loader_id) => {
                if let Some(name) = options.wait_until.lifecycle_name() {
                    self.page
                        .wait_for_lifecycle(&mut events, &loader_id, name, &url, timeout)
                        .await?;
                }
                self.page
                    .inner
                    .state
                    .lock()
                    .unwrap()
                    .take_response(&loader_id)
            }
        };
        self.page
            .fire_hooks_after_navigation(response.as_ref())
            .await;
        Ok(response)
    }

    /// Waits until this frame's current document has reached `state`.
    pub async fn wait_for_load_state(
        &self,
        state: WaitUntil,
        timeout: Option<Duration>,
    ) -> Result<()> {
        self.page.ensure_open()?;
        let Some(name) = state.lifecycle_name() else {
            return Ok(());
        };
        let timeout = timeout.unwrap_or(self.page.inner.context.options().navigation_timeout);
        let deadline = Instant::now() + timeout;
        let mut events = self.page.inner.events.subscribe();
        loop {
            let loader = self
                .data(|f| f.loader_id.clone())
                .ok_or_else(|| self.detached_error())?;
            if let Some(loader) = &loader
                && self.page.inner.state.lock().unwrap().reached(loader, name)
            {
                return Ok(());
            }
            // An out-of-process frame adopted after it loaded never reports its lifecycle; ask
            // the document. (Elsewhere the blank document before a navigation would answer.)
            if name != "networkIdle"
                && self.is_out_of_process()
                && let Ok(ready) = self.evaluate::<String>("document.readyState").await
                && (ready == "complete" || (name == "DOMContentLoaded" && ready == "interactive"))
            {
                return Ok(());
            }
            let poll = (Instant::now() + Duration::from_millis(100)).min(deadline);
            match self.page.next_event(&mut events, poll).await {
                Some(PageEvent::Lifecycle {
                    frame_id, name: n, ..
                }) if frame_id == self.id && n == name => {
                    return Ok(());
                }
                Some(PageEvent::Closed) => return Err(self.page.closed_error()),
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(crate::page::navigation_timeout(&self.url(), timeout));
            }
        }
    }

    /// Waits until this frame's URL matches.
    pub async fn wait_for_url(
        &self,
        matcher: impl Into<UrlMatcher>,
        timeout: Option<Duration>,
    ) -> Result<()> {
        let matcher = matcher.into();
        let timeout = timeout.unwrap_or(self.page.inner.context.options().navigation_timeout);
        let mut events = self.page.inner.events.subscribe();
        let deadline = Instant::now() + timeout;
        loop {
            if matcher.matches(&self.url()) {
                return Ok(());
            }
            let poll = (Instant::now() + Duration::from_millis(100)).min(deadline);
            match self.page.next_event(&mut events, poll).await {
                Some(PageEvent::Navigated { frame_id, url, .. })
                    if frame_id == self.id && matcher.matches(&url) =>
                {
                    return Ok(());
                }
                Some(PageEvent::Closed) => return Err(self.page.closed_error()),
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

    /// A locator for `selector` inside this frame.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::in_frame(self.page.clone(), self.id.clone(), selector)
    }

    /// A locator for the element the browser knows by `backend_node_id` in this frame's
    /// document, as [`AxNode::backend_node_id`](crate::accessibility::AxNode) gives it.
    pub fn locator_by_backend_node_id(&self, backend_node_id: i64) -> Locator {
        self.locator(&format!("backend-node-id={backend_node_id}"))
    }

    /// Locates elements inside an iframe within this frame.
    pub fn frame_locator(&self, selector: &str) -> FrameLocator {
        FrameLocator::new(self.locator(selector))
    }

    /// Where this frame's viewport sits in the page's viewport, in CSS pixels: the sum of
    /// the content-box origins of the `<iframe>` elements holding it.
    pub(crate) async fn origin(&self) -> Result<(f64, f64)> {
        let (mut x, mut y) = (0.0, 0.0);
        let mut current = self.clone();
        while let Some(parent) = current.parent_frame() {
            let owner = current.owner_element(&parent).await?;
            let (ox, oy): (f64, f64) = owner
                .evaluate(
                    "function() { var r = this.getBoundingClientRect(); var s = getComputedStyle(this); \
                     return [r.left + this.clientLeft + (parseFloat(s.paddingLeft) || 0), \
                             r.top + this.clientTop + (parseFloat(s.paddingTop) || 0)]; }",
                    &[],
                )
                .await?;
            x += ox;
            y += oy;
            current = parent;
        }
        Ok((x, y))
    }

    /// Scrolls every `<iframe>` holding this frame into view, outermost last. Returns
    /// whether anything moved.
    pub(crate) async fn scroll_into_view(&self, scroll_js: &str) -> Result<bool> {
        let mut scrolled = false;
        let mut current = self.clone();
        while let Some(parent) = current.parent_frame() {
            let owner = current.owner_element(&parent).await?;
            let moved = call_function(
                &owner.session,
                owner.object_id(),
                scroll_js,
                &[],
                true,
                false,
            )
            .await?;
            scrolled |= moved.value == Some(serde_json::Value::Bool(true));
            current = parent;
        }
        Ok(scrolled)
    }

    /// Waits for this frame and every frame holding it to render twice, so a scroll has
    /// reached the compositor that routes input to out-of-process frames.
    pub(crate) async fn settle_rendering(&self) -> Result<()> {
        let mut current = Some(self.clone());
        while let Some(frame) = current {
            frame
                .evaluate::<()>(
                    "new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r())))",
                )
                .await?;
            current = frame.parent_frame();
        }
        Ok(())
    }

    /// The `<iframe>` element in `parent` that holds this frame.
    async fn owner_element(&self, parent: &Frame) -> Result<ElementHandle> {
        let session = parent.session();
        let owner = session
            .send(&dom::GetFrameOwnerParams {
                frame_id: self.id.clone(),
            })
            .await?;
        let resolved = session
            .send(&dom::ResolveNodeParams {
                backend_node_id: Some(owner.backend_node_id),
                ..Default::default()
            })
            .await?;
        let object_id = resolved
            .object
            .object_id
            .ok_or_else(|| self.detached_error())?;
        Ok(ElementHandle::new(session, object_id))
    }
}

impl_get_by!(Frame);

/// Locates elements inside an `<iframe>`, finding the iframe afresh each time it is used.
///
/// ```no_run
/// # async fn example(page: agito::Page) -> agito::Result<()> {
/// let payment = page.frame_locator("iframe#payment");
/// payment.get_by_label("Card number").fill("4242 4242 4242 4242").await?;
/// payment.get_by_role("button", Some("Pay")).click().await?;
/// # Ok(()) }
/// ```
#[derive(Clone, Debug)]
#[must_use = "a frame locator does nothing until a locator built from it is used"]
pub struct FrameLocator {
    owner: Locator,
}

impl FrameLocator {
    pub(crate) fn new(owner: Locator) -> Self {
        Self { owner }
    }

    /// The locator for the `<iframe>` element itself.
    pub fn owner(&self) -> &Locator {
        &self.owner
    }

    /// Elements matching `selector` inside the iframe.
    pub fn locator(&self, selector: &str) -> Locator {
        Locator::in_frame_owner(self.owner.clone(), selector)
    }

    /// An iframe nested inside this one.
    pub fn frame_locator(&self, selector: &str) -> FrameLocator {
        FrameLocator::new(self.locator(selector))
    }

    /// The first iframe the owner selector matches.
    pub fn first(&self) -> FrameLocator {
        FrameLocator::new(self.owner.first())
    }

    /// The last iframe the owner selector matches.
    pub fn last(&self) -> FrameLocator {
        FrameLocator::new(self.owner.last())
    }

    /// The `index`-th iframe the owner selector matches.
    pub fn nth(&self, index: usize) -> FrameLocator {
        FrameLocator::new(self.owner.nth(index))
    }

    /// The frame the iframe currently holds.
    pub async fn frame(&self) -> Result<Frame> {
        self.owner.content_frame_now().await
    }
}

impl_get_by!(FrameLocator);

/// The id of the frame an `<iframe>` element holds, or `None` for any other element.
pub(crate) async fn content_frame_id(handle: &ElementHandle) -> Result<Option<String>> {
    let described = handle
        .session
        .send(&dom::DescribeNodeParams {
            object_id: Some(handle.object_id().to_owned()),
            ..Default::default()
        })
        .await?;
    Ok(described.node.frame_id)
}

/// Whether the caller asked for `()`, so the value need not be serialized at all (it may not
/// be serializable, like a `Window`).
fn discards<T>() -> bool {
    std::any::type_name::<T>() == "()"
}

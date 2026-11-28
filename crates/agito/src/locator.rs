//! Locators: lazy, re-resolving descriptions of elements, with auto-waiting actions.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, ErrorKind, Result};
use crate::frame::{Frame, FrameLocator, content_frame_id};
use crate::handles::{BoundingBox, ElementHandle, call_function, decode_value};
use crate::input::ClickOptions;
use crate::page::Page;
use crate::plugin::ActionResult;
use crate::selectors::{css_string, parse_prefix};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

const IS_VISIBLE: &str = include_str!("../js/is_visible.js");
const IS_ENABLED: &str = include_str!("../js/is_enabled.js");
const IS_EDITABLE: &str = include_str!("../js/is_editable.js");
const SCROLL_INTO_VIEW: &str = include_str!("../js/scroll_into_view_if_needed.js");
const MEASURE_DRIFT: &str = include_str!("../js/measure_drift.js");
const RECEIVES_EVENTS: &str = include_str!("../js/receives_events.js");
const COVERING_ELEMENT: &str = include_str!("../js/covering_element.js");
const READ_CHECKED: &str = include_str!("../js/read_checked_state.js");
const FILL: &str = include_str!("../js/fill.js");
const SELECT_OPTIONS: &str = include_str!("../js/select_options.js");
const QUERY_SHADOW: &str = include_str!("../js/query_shadow.js");

/// Which actionability checks an action waits for.
#[derive(Debug, Clone, Copy, Default)]
struct Checks {
    visible: bool,
    enabled: bool,
    editable: bool,
    scroll: bool,
    stable: bool,
    receives_events: bool,
}

const POINTER: Checks = Checks {
    visible: true,
    enabled: true,
    editable: false,
    scroll: true,
    stable: true,
    receives_events: true,
};
const EDIT: Checks = Checks {
    visible: true,
    enabled: true,
    editable: true,
    scroll: false,
    stable: false,
    receives_events: false,
};
const FOCUS: Checks = Checks {
    visible: true,
    enabled: true,
    editable: false,
    scroll: false,
    stable: false,
    receives_events: false,
};
const ATTACHED: Checks = Checks {
    visible: false,
    enabled: false,
    editable: false,
    scroll: false,
    stable: false,
    receives_events: false,
};

/// A state for [`Locator::wait_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ElementState {
    /// Present in the document.
    Attached,
    /// Not in the document.
    Detached,
    /// Visible.
    #[default]
    Visible,
    /// Hidden, or not in the document.
    Hidden,
}

/// One refinement applied after the base selector, in the order written.
#[derive(Debug, Clone)]
enum Step {
    /// Descendants matching a CSS selector.
    Child(String),
    /// Walk up this many parents.
    Parent(u32),
    /// Keep matches whose text contains this.
    HasText(String),
    /// Keep one match: index from zero, or -1 for the last.
    Nth(i64),
}

/// Finds elements when used, never in advance, so it survives re-renders. Actions wait for the
/// element to be actionable (attached, visible, enabled, stable, not covered) before acting.
///
/// Refinements (`locator`, `filter_has_text`, `first`, `nth`, ...) apply in the order they
/// are chained, and a locator that matches several elements acts on the first.
///
/// ```no_run
/// # async fn example(page: agito::Page) -> agito::Result<()> {
/// use agito::expect;
///
/// let products = page.locator(".product");
/// let mug = products.filter_has_text("mug");
/// mug.locator("button")?.click().await?;
/// expect(&page.locator("#cart-count")).to_have_text("1").await?;
/// # Ok(()) }
/// ```
#[derive(Clone)]
#[must_use = "a locator does nothing until an action or query runs on it"]
pub struct Locator {
    page: Page,
    root: Root,
    selector: String,
    steps: Vec<Step>,
    pierce_shadow: bool,
    timeout: Option<Duration>,
}

/// Where a locator's base selector is resolved.
#[derive(Clone)]
enum Root {
    /// The page's main frame.
    Main,
    /// A frame known by id.
    Frame(String),
    /// Whatever frame the `<iframe>` this locator points at currently holds.
    Owner(Box<Locator>),
}

impl std::fmt::Debug for Locator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Locator({})", self.description())
    }
}

impl Locator {
    pub(crate) fn new(page: Page, selector: &str) -> Self {
        Self::rooted(page, Root::Main, selector)
    }

    pub(crate) fn in_frame(page: Page, frame_id: String, selector: &str) -> Self {
        Self::rooted(page, Root::Frame(frame_id), selector)
    }

    pub(crate) fn in_frame_owner(owner: Locator, selector: &str) -> Self {
        Self::rooted(owner.page.clone(), Root::Owner(Box::new(owner)), selector)
    }

    fn rooted(page: Page, root: Root, selector: &str) -> Self {
        Self {
            page,
            root,
            selector: selector.to_owned(),
            steps: Vec::new(),
            pierce_shadow: true,
            timeout: None,
        }
    }

    /// The page this locator searches.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The selector this locator was built from.
    pub fn selector(&self) -> &str {
        &self.selector
    }

    /// A readable description including chained steps, for messages.
    pub fn description(&self) -> String {
        let mut s = match &self.root {
            Root::Main => String::new(),
            Root::Frame(id) => format!("frame[{id}] >> "),
            Root::Owner(owner) => format!("{} >> content-frame >> ", owner.description()),
        };
        s.push_str(&self.selector);
        for step in &self.steps {
            match step {
                Step::Child(css) => s.push_str(&format!(" >> {css}")),
                Step::Parent(n) => s.push_str(&" >> ..".repeat(*n as usize)),
                Step::HasText(t) => s.push_str(&format!(" >> has-text={t:?}")),
                Step::Nth(-1) => s.push_str(" >> last"),
                Step::Nth(n) => s.push_str(&format!(" >> nth={n}")),
            }
        }
        s
    }

    fn with_step(&self, step: Step) -> Locator {
        let mut next = self.clone();
        next.steps.push(step);
        next
    }

    /// Locates elements inside the `<iframe>` this locator points at.
    pub fn content_frame(&self) -> FrameLocator {
        FrameLocator::new(self.clone())
    }

    /// The frame this locator's selector is resolved in. Boxed because a frame locator
    /// nests locators, so resolution recurses.
    fn resolve_frame(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<Frame>> + Send + '_>> {
        Box::pin(async move {
            match &self.root {
                Root::Main => Ok(self.page.main_frame()),
                Root::Frame(id) => self.page.frame_by_id(id).ok_or_else(|| {
                    ErrorKind::Evaluation {
                        message: format!("Frame {id} was detached from the page."),
                    }
                    .into()
                }),
                Root::Owner(owner) => owner.content_frame_now().await,
            }
        })
    }

    /// The frame held by the `<iframe>` this locator points at right now.
    pub(crate) async fn content_frame_now(&self) -> Result<Frame> {
        let handle = self.resolve_now().await?;
        let frame_id = content_frame_id(&handle).await?.ok_or_else(|| {
            Error::invalid_argument(format!(
                "'{}' is not an <iframe> or <frame>, so it holds no frame.",
                self.description()
            ))
        })?;
        // An out-of-process iframe is known to the page a moment after its element exists.
        self.page.frame_by_id(&frame_id).ok_or_else(|| {
            ErrorKind::Evaluation {
                message: format!(
                    "The frame inside '{}' is not attached yet.",
                    self.description()
                ),
            }
            .into()
        })
    }

    /// The first match.
    pub fn first(&self) -> Locator {
        self.with_step(Step::Nth(0))
    }

    /// The last match.
    pub fn last(&self) -> Locator {
        self.with_step(Step::Nth(-1))
    }

    /// The `index`-th match, from zero.
    pub fn nth(&self, index: usize) -> Locator {
        self.with_step(Step::Nth(index as i64))
    }

    /// Only matches whose text contains `text`.
    pub fn filter_has_text(&self, text: &str) -> Locator {
        self.with_step(Step::HasText(text.to_owned()))
    }

    /// Whether CSS and text selectors search inside shadow roots (the default).
    pub fn pierce_shadow(&self, pierce: bool) -> Locator {
        Locator {
            pierce_shadow: pierce,
            ..self.clone()
        }
    }

    /// Overrides the action timeout for this locator.
    pub fn with_timeout(&self, timeout: Duration) -> Locator {
        Locator {
            timeout: Some(timeout),
            ..self.clone()
        }
    }

    /// Descendants matching a CSS `selector`. A leading `..` (or `../..`) steps up to parents
    /// first; `..` alone is the parent.
    pub fn locator(&self, selector: &str) -> Result<Locator> {
        let (parents, rest) = parse_parent_prefix(selector);
        if parents == 0 && rest.trim().is_empty() {
            return Err(Error::invalid_argument(
                "An empty selector matches nothing.",
            ));
        }
        let mut next = self.clone();
        if parents > 0 {
            next.steps.push(Step::Parent(parents));
        }
        if !rest.is_empty() {
            next.steps.push(Step::Child(rest.to_owned()));
        }
        Ok(next)
    }

    /// Descendants with a `data-testid`.
    pub fn get_by_test_id(&self, test_id: &str) -> Result<Locator> {
        self.locator(&crate::selectors::test_id_selector(test_id))
    }

    fn action_timeout(&self) -> Duration {
        self.timeout
            .unwrap_or(self.page.context().options().action_timeout)
    }

    /// Every element this locator currently matches. Also returns a note on which step
    /// emptied the set, for not-found messages.
    async fn resolve_all(&self) -> Result<(Vec<ElementHandle>, Option<String>)> {
        let (prefix, expression) = parse_prefix(&self.selector);
        let strategy = self
            .page
            .context()
            .inner
            .selectors
            .get(prefix)
            .ok_or_else(|| {
                Error::invalid_argument(format!(
                    "No selector strategy registered for prefix: {prefix}"
                ))
            })?;
        let frame = self.resolve_frame().await?;
        let mut handles = strategy
            .resolve(expression, &frame, self.pierce_shadow)
            .await?;
        let session = &frame.session();
        for step in &self.steps {
            if handles.is_empty() {
                break;
            }
            let before = handles.len();
            handles = match step {
                Step::Nth(n) => pick(&handles, *n).into_iter().collect(),
                Step::HasText(text) => {
                    let mut kept = Vec::new();
                    for h in handles {
                        if h.text_content()
                            .await?
                            .is_some_and(|t| t.contains(text.as_str()))
                        {
                            kept.push(h);
                        }
                    }
                    kept
                }
                Step::Parent(n) => {
                    let mut walked = Vec::new();
                    for h in &handles {
                        let parent = call_function(
                            session,
                            h.object_id(),
                            "function(n) { var e = this; for (var i = 0; i < n && e; i++) e = e.parentElement; return e; }",
                            &[Value::from(*n)],
                            false,
                            false,
                        )
                        .await?;
                        if let Some(id) = parent.object_id {
                            walked.push(ElementHandle::new(session.clone(), id));
                        }
                    }
                    dedupe(walked, session).await?
                }
                Step::Child(css) => {
                    let sel = Value::from(css.as_str()).to_string();
                    let function = if self.pierce_shadow {
                        format!("function() {{ {QUERY_SHADOW} return queryShadow(this, {sel}); }}")
                    } else {
                        format!("function() {{ return Array.from(this.querySelectorAll({sel})); }}")
                    };
                    let mut found = Vec::new();
                    for h in &handles {
                        let array =
                            call_function(session, h.object_id(), &function, &[], false, false)
                                .await?;
                        found.extend(crate::handles::array_to_handles(session, array).await?);
                    }
                    let found = dedupe(found, session).await?;
                    if found.is_empty() {
                        return Ok((
                            found,
                            Some(format!(
                                "{before} element(s) matched before '{css}', and none of them contain a match for it."
                            )),
                        ));
                    }
                    found
                }
            };
        }
        Ok((handles, None))
    }

    /// The element this locator points at right now: the first match.
    pub(crate) async fn resolve_now(&self) -> Result<ElementHandle> {
        let (handles, note) = self.resolve_all().await?;
        handles.into_iter().next().ok_or_else(|| {
            ErrorKind::ElementNotFound {
                selector: self.description(),
                page_url: self.page.url(),
                dom_snapshot: note,
            }
            .into()
        })
    }

    async fn eval<T: serde::de::DeserializeOwned>(
        &self,
        handle: &ElementHandle,
        function: &str,
        args: &[Value],
    ) -> Result<T> {
        let result = call_function(
            &handle.session,
            handle.object_id(),
            function,
            args,
            true,
            true,
        )
        .await?;
        decode_value(result)
    }

    /// Resolves the element and waits until it passes `checks`.
    async fn wait_actionable(&self, checks: Checks, timeout: Duration) -> Result<ElementHandle> {
        let deadline = Instant::now() + timeout;
        let mut last_check: &str;
        let mut last_detail: Option<String>;
        loop {
            match self.check_once(checks).await {
                Ok(Ok(handle)) => return Ok(handle),
                Ok(Err((check, detail))) => {
                    last_check = check;
                    last_detail = detail;
                }
                Err(e) if is_retryable(&e) => {
                    last_check = "resolve";
                    last_detail = None;
                }
                Err(e) => return Err(e),
            }
            if Instant::now() + POLL_INTERVAL > deadline {
                let mut message = format!(
                    "Actionability check '{last_check}' timed out for selector '{}'.",
                    self.description()
                );
                if let Some(detail) = &last_detail {
                    message.push(' ');
                    message.push_str(detail);
                }
                return Err(ErrorKind::ActionTimeout {
                    selector: self.description(),
                    failed_check: last_check.to_owned(),
                    element_state: last_detail,
                    page_url: self.page.url(),
                    timeout,
                    message,
                }
                .into());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// One pass of the checks: the element, or the check that failed and why.
    async fn check_once(
        &self,
        checks: Checks,
    ) -> Result<std::result::Result<ElementHandle, (&'static str, Option<String>)>> {
        let handle = self.resolve_now().await?;
        if checks.visible && !self.eval::<bool>(&handle, IS_VISIBLE, &[]).await? {
            return Ok(Err((
                "visible",
                Some("element was found but is not visible (display:none, visibility:hidden, zero size, or opacity:0)".into()),
            )));
        }
        if checks.enabled && !self.eval::<bool>(&handle, IS_ENABLED, &[]).await? {
            return Ok(Err((
                "enabled",
                Some("element was found but is disabled".into()),
            )));
        }
        if checks.editable && !self.eval::<bool>(&handle, IS_EDITABLE, &[]).await? {
            return Ok(Err((
                "editable",
                Some("element was found but is not editable".into()),
            )));
        }
        if checks.scroll {
            let mut scrolled: bool = self.eval(&handle, SCROLL_INTO_VIEW, &[]).await?;
            let frame = self.resolve_frame().await?;
            if !frame.is_main() {
                scrolled |= frame.scroll_into_view(SCROLL_INTO_VIEW).await?;
                if scrolled {
                    // Input reaches an out-of-process frame through the compositor, which can
                    // still hold the layout from before the scroll. Let it catch up, then
                    // measure again.
                    frame.settle_rendering().await?;
                    return Ok(Err((
                        "scrollIntoView",
                        Some(
                            "element was scrolled into view; waiting for the page to settle".into(),
                        ),
                    )));
                }
            }
        }
        if checks.stable {
            let drift: f64 = self.eval(&handle, MEASURE_DRIFT, &[]).await?;
            if drift >= 2.0 {
                return Ok(Err((
                    "stable",
                    Some(format!(
                        "element was located but kept moving (last measured drift: {drift:.1}px across x/y/width/height over 50ms)"
                    )),
                )));
            }
        }
        if checks.receives_events && !self.eval::<bool>(&handle, RECEIVES_EVENTS, &[]).await? {
            let covering: String = self
                .eval(&handle, COVERING_ELEMENT, &[])
                .await
                .unwrap_or_else(|_| "unknown".into());
            return Ok(Err((
                "receivesEvents",
                Some(format!(
                    "element was found but is covered at its center point by <{covering}> which intercepts pointer events"
                )),
            )));
        }
        Ok(Ok(handle))
    }

    /// Runs an action between the context's before/after action hooks.
    async fn run_action<T>(
        &self,
        name: &str,
        action: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let hooks = self.page.context().inner.hooks.clone();
        for hook in &hooks {
            if let Err(e) = hook.before_action(&self.page, name).await {
                tracing::warn!(error = %e, "lifecycle hook before_action failed");
            }
        }
        let result = action.await;
        let summary = ActionResult {
            action: name.to_owned(),
            error: result.as_ref().err().map(|e| e.to_string()),
        };
        for hook in &hooks {
            if let Err(e) = hook.after_action(&self.page, &summary).await {
                tracing::warn!(error = %e, "lifecycle hook after_action failed");
            }
        }
        result
    }

    async fn center(&self, handle: &ElementHandle) -> Result<(f64, f64)> {
        let bbox = handle
            .bounding_box()
            .await?
            .ok_or_else(|| ErrorKind::Evaluation {
                message: "Element has zero size bounding box.".into(),
            })?;
        let (x, y) = bbox.center();
        let (ox, oy) = self.resolve_frame().await?.origin().await?;
        Ok((x + ox, y + oy))
    }

    /// Clicks the element's center.
    pub async fn click(&self) -> Result<()> {
        self.click_with(ClickOptions::default()).await
    }

    /// Clicks with a given button, modifiers or click count.
    pub async fn click_with(&self, options: ClickOptions) -> Result<()> {
        self.run_action("click", async {
            let handle = self.wait_actionable(POINTER, self.action_timeout()).await?;
            let (x, y) = self.center(&handle).await?;
            self.page.mouse().click(x, y, options).await
        })
        .await
    }

    /// Double-clicks the element's center.
    pub async fn dblclick(&self) -> Result<()> {
        self.run_action("dblclick", async {
            let handle = self.wait_actionable(POINTER, self.action_timeout()).await?;
            let (x, y) = self.center(&handle).await?;
            self.page
                .mouse()
                .dblclick(x, y, ClickOptions::default())
                .await
        })
        .await
    }

    /// Moves the mouse over the element.
    pub async fn hover(&self) -> Result<()> {
        self.run_action("hover", async {
            let handle = self.wait_actionable(POINTER, self.action_timeout()).await?;
            let (x, y) = self.center(&handle).await?;
            self.page.mouse().move_to(x, y, 1).await
        })
        .await
    }

    /// Focuses the element.
    pub async fn focus(&self) -> Result<()> {
        self.run_action("focus", async {
            let handle = self
                .wait_actionable(ATTACHED, self.action_timeout())
                .await?;
            self.eval::<Value>(&handle, "function() { this.focus(); }", &[])
                .await
                .map(|_| ())
        })
        .await
    }

    /// Replaces the element's value with `value`: an input or textarea's value, firing its
    /// `input` and `change` events, or a `contenteditable` element's contents, inserted as if
    /// typed.
    pub async fn fill(&self, value: &str) -> Result<()> {
        self.run_action("fill", async {
            let handle = self.wait_actionable(EDIT, self.action_timeout()).await?;
            self.fill_handle(&handle, value).await
        })
        .await
    }

    /// Empties an input, a textarea or a `contenteditable` element.
    pub async fn clear(&self) -> Result<()> {
        self.run_action("clear", async {
            let handle = self.wait_actionable(EDIT, self.action_timeout()).await?;
            self.fill_handle(&handle, "").await
        })
        .await
    }

    async fn fill_handle(&self, handle: &ElementHandle, value: &str) -> Result<()> {
        let kind: String = self.eval(handle, FILL, &[Value::from(value)]).await?;
        if kind == "editable" {
            // The script selected the contents; replace the selection.
            if value.is_empty() {
                self.page.keyboard().press("Delete").await?;
            } else {
                self.page.keyboard().insert_text(value).await?;
            }
        }
        Ok(())
    }

    /// Focuses the element and types `text` key by key.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.run_action("type", async {
            let handle = self.wait_actionable(EDIT, self.action_timeout()).await?;
            self.eval::<Value>(&handle, "function() { this.focus(); }", &[])
                .await?;
            self.page.keyboard().type_text(text).await
        })
        .await
    }

    /// Focuses the element and presses a key or chord (`Enter`, `Control+A`).
    pub async fn press(&self, key: &str) -> Result<()> {
        self.run_action("press", async {
            let handle = self.wait_actionable(FOCUS, self.action_timeout()).await?;
            self.eval::<Value>(&handle, "function() { this.focus(); }", &[])
                .await?;
            self.page.keyboard().press(key).await
        })
        .await
    }

    /// Checks a checkbox or radio button.
    pub async fn check(&self) -> Result<()> {
        self.set_checked(true).await
    }

    /// Unchecks a checkbox.
    pub async fn uncheck(&self) -> Result<()> {
        self.set_checked(false).await
    }

    /// Clicks the element if its checked state differs from `checked`, then verifies it.
    pub async fn set_checked(&self, checked: bool) -> Result<()> {
        let name = if checked { "check" } else { "uncheck" };
        self.run_action(name, async {
            let timeout = self.action_timeout();
            let handle = self.wait_actionable(FOCUS, timeout).await?;
            let (is_radio, is_checked) = self.read_checked(&handle).await?;
            if is_checked == checked {
                return Ok(());
            }
            if is_radio && !checked {
                return Err(Error::invalid_argument(format!(
                    "Cannot uncheck '{}': it is a radio button, which is only unchecked by checking another in its group.",
                    self.description()
                )));
            }
            let handle = self.wait_actionable(POINTER, timeout).await?;
            let (x, y) = self.center(&handle).await?;
            self.page.mouse().click(x, y, ClickOptions::default()).await?;
            let Ok((_, now)) = self.read_checked(&handle).await else { return Ok(()) };
            if now != checked {
                return Err(Error::other(format!(
                    "Clicking '{}' did not {name} it: it is still {}. A handler may have cancelled the click, or the page may have reset the state.",
                    self.description(),
                    if now { "checked" } else { "unchecked" }
                )));
            }
            Ok(())
        })
        .await
    }

    async fn read_checked(&self, handle: &ElementHandle) -> Result<(bool, bool)> {
        let state: Option<String> = self.eval(handle, READ_CHECKED, &[]).await?;
        let state = state.ok_or_else(|| {
            Error::invalid_argument(format!(
                "Cannot check or uncheck '{}': it is not a checkbox, a radio button, or an element with a checkable role.",
                self.description()
            ))
        })?;
        Ok((state.starts_with("radio:"), state.ends_with(":true")))
    }

    /// Selects `<option>`s by value or label; returns the selected values.
    pub async fn select_option(&self, values: &[&str]) -> Result<Vec<String>> {
        self.run_action("selectOption", async {
            let handle = self.wait_actionable(FOCUS, self.action_timeout()).await?;
            self.eval(&handle, SELECT_OPTIONS, &[Value::from(values.to_vec())])
                .await
        })
        .await
    }

    /// Puts files into this `<input type=file>`, firing `input` and `change`. Takes paths on
    /// disk or in-memory [`FilePayload`](crate::files::FilePayload)s; an empty list clears the
    /// input. The input may be hidden, as styled upload buttons usually hide it.
    pub async fn set_input_files(&self, files: impl Into<crate::files::InputFiles>) -> Result<()> {
        let files = files.into();
        self.run_action("setInputFiles", async {
            let handle = self.wait_actionable(ATTACHED, self.action_timeout()).await?;
            let (is_file_input, multiple): (bool, bool) = self
                .eval_tuple(
                    &handle,
                    "function() { return [this.tagName === 'INPUT' && this.type === 'file', !!this.multiple]; }",
                )
                .await?;
            if !is_file_input {
                return Err(Error::invalid_argument(format!(
                    "'{}' is not an <input type=file>.",
                    self.description()
                )));
            }
            let count = match &files {
                crate::files::InputFiles::Paths(p) => p.len(),
                crate::files::InputFiles::Payloads(p) => p.len(),
            };
            if count > 1 && !multiple {
                return Err(Error::invalid_argument(format!(
                    "'{}' accepts a single file; add the `multiple` attribute to accept {count}.",
                    self.description()
                )));
            }
            let paths = crate::files::materialize(files.clone(), || self.page.upload_dir())?;
            crate::files::set_input_files(
                &handle.session,
                crate::files::FileInputTarget::Object(handle.object_id().to_owned()),
                paths,
            )
            .await
        })
        .await
    }

    async fn eval_tuple<T: serde::de::DeserializeOwned>(
        &self,
        handle: &ElementHandle,
        function: &str,
    ) -> Result<T> {
        self.eval(handle, function, &[]).await
    }

    /// Scrolls the element into view if it is not.
    pub async fn scroll_into_view(&self) -> Result<()> {
        let handle = self
            .wait_actionable(ATTACHED, self.action_timeout())
            .await?;
        self.eval::<bool>(&handle, SCROLL_INTO_VIEW, &[])
            .await
            .map(|_| ())
    }

    /// Waits until the element reaches `state`.
    pub async fn wait_for(&self, state: ElementState, timeout: Option<Duration>) -> Result<()> {
        let timeout = timeout.unwrap_or(self.action_timeout());
        let deadline = Instant::now() + timeout;
        loop {
            let reached = match state {
                ElementState::Attached => self.resolve_now().await.is_ok(),
                ElementState::Detached => {
                    matches!(self.resolve_now().await, Err(e) if matches!(e.kind(), ErrorKind::ElementNotFound { .. }))
                }
                ElementState::Visible => self.is_visible().await.unwrap_or(false),
                ElementState::Hidden => self.is_hidden().await.unwrap_or(false),
            };
            if reached {
                return Ok(());
            }
            if Instant::now() + POLL_INTERVAL > deadline {
                return Err(ErrorKind::WaitTimeout {
                    condition: format!("{state:?}").to_lowercase(),
                    timeout,
                    last_evaluated_value: None,
                    message: format!(
                        "Waiting for '{}' to be {} timed out after {}ms.",
                        self.description(),
                        format!("{state:?}").to_lowercase(),
                        timeout.as_millis()
                    ),
                }
                .into());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn attached(&self) -> Result<ElementHandle> {
        self.wait_actionable(ATTACHED, self.action_timeout()).await
    }

    /// `textContent`, once the element exists.
    pub async fn text_content(&self) -> Result<Option<String>> {
        self.attached().await?.text_content().await
    }

    /// `innerText`, once the element exists.
    pub async fn inner_text(&self) -> Result<String> {
        self.attached().await?.inner_text().await
    }

    /// `innerHTML`, once the element exists.
    pub async fn inner_html(&self) -> Result<String> {
        self.attached()
            .await?
            .evaluate("function() { return this.innerHTML; }", &[])
            .await
    }

    /// An attribute, once the element exists.
    pub async fn get_attribute(&self, name: &str) -> Result<Option<String>> {
        self.attached().await?.get_attribute(name).await
    }

    /// An input's current value, once the element exists.
    pub async fn input_value(&self) -> Result<String> {
        let handle = self.attached().await?;
        let value: Option<String> = handle
            .evaluate("function() { return this.value; }", &[])
            .await?;
        value.ok_or_else(|| {
            Error::invalid_argument(format!(
                "'{}' is not an input, textarea or select.",
                self.description()
            ))
        })
    }

    /// The element's box, or `None` if it has no size.
    pub async fn bounding_box(&self) -> Result<Option<BoundingBox>> {
        let Some(mut bbox) = self.attached().await?.bounding_box().await? else {
            return Ok(None);
        };
        let (ox, oy) = self.resolve_frame().await?.origin().await?;
        bbox.x += ox;
        bbox.y += oy;
        Ok(Some(bbox))
    }

    /// Calls a function with the element as `this`.
    pub async fn evaluate<T: serde::de::DeserializeOwned>(
        &self,
        function: &str,
        args: &[Value],
    ) -> Result<T> {
        self.attached().await?.evaluate(function, args).await
    }

    /// The element itself, once it exists.
    pub async fn element_handle(&self) -> Result<ElementHandle> {
        self.attached().await
    }

    /// How many elements match right now.
    pub async fn count(&self) -> Result<usize> {
        let (handles, _) = self.resolve_all().await?;
        Ok(handles.len())
    }

    /// A locator for each element matching right now.
    pub async fn all(&self) -> Result<Vec<Locator>> {
        Ok((0..self.count().await?).map(|i| self.nth(i)).collect())
    }

    /// The text of every element matching right now.
    pub async fn all_text_contents(&self) -> Result<Vec<String>> {
        let (handles, _) = self.resolve_all().await?;
        let mut texts = Vec::with_capacity(handles.len());
        for h in handles {
            texts.push(h.text_content().await?.unwrap_or_default());
        }
        Ok(texts)
    }

    /// The `innerText` of every element matching right now.
    pub async fn all_inner_texts(&self) -> Result<Vec<String>> {
        let (handles, _) = self.resolve_all().await?;
        let mut texts = Vec::with_capacity(handles.len());
        for h in handles {
            texts.push(h.inner_text().await?);
        }
        Ok(texts)
    }

    /// Whether the element exists and is visible. Does not wait.
    pub async fn is_visible(&self) -> Result<bool> {
        match self.resolve_now().await {
            Ok(handle) => self.eval(&handle, IS_VISIBLE, &[]).await,
            Err(e) if matches!(e.kind(), ErrorKind::ElementNotFound { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Whether the element is absent or not visible. Does not wait.
    pub async fn is_hidden(&self) -> Result<bool> {
        Ok(!self.is_visible().await?)
    }

    /// Whether the element is enabled, once it exists.
    pub async fn is_enabled(&self) -> Result<bool> {
        let handle = self.attached().await?;
        self.eval(&handle, IS_ENABLED, &[]).await
    }

    /// Whether the element is disabled, once it exists.
    pub async fn is_disabled(&self) -> Result<bool> {
        Ok(!self.is_enabled().await?)
    }

    /// Whether the element is editable, once it exists.
    pub async fn is_editable(&self) -> Result<bool> {
        let handle = self.attached().await?;
        self.eval(&handle, IS_EDITABLE, &[]).await
    }

    /// Whether a checkbox or radio button is checked, once it exists.
    pub async fn is_checked(&self) -> Result<bool> {
        let handle = self.attached().await?;
        Ok(self.read_checked(&handle).await?.1)
    }

    // Non-waiting probes used by `expect`, which does its own retrying.

    pub(crate) async fn probe_text(&self) -> Result<String> {
        Ok(self
            .resolve_now()
            .await?
            .text_content()
            .await?
            .unwrap_or_default())
    }

    pub(crate) async fn probe_eval<T: serde::de::DeserializeOwned>(
        &self,
        function: &str,
        args: &[Value],
    ) -> Result<T> {
        let handle = self.resolve_now().await?;
        self.eval(&handle, function, args).await
    }

    pub(crate) async fn probe_checked(&self) -> Result<bool> {
        let handle = self.resolve_now().await?;
        Ok(self.read_checked(&handle).await?.1)
    }

    pub(crate) const VISIBLE_JS: &'static str = IS_VISIBLE;
    pub(crate) const ENABLED_JS: &'static str = IS_ENABLED;
    pub(crate) const EDITABLE_JS: &'static str = IS_EDITABLE;

    /// A CSS attribute selector for `name="value"`, for building selectors safely.
    pub fn attribute_selector(name: &str, value: &str) -> String {
        format!("[{name}={}]", css_string(value))
    }
}

fn pick(handles: &[ElementHandle], n: i64) -> Option<ElementHandle> {
    if n == -1 {
        handles.last().cloned()
    } else {
        usize::try_from(n)
            .ok()
            .and_then(|i| handles.get(i).cloned())
    }
}

/// Removes handles pointing at the same DOM node, keeping the first.
async fn dedupe(
    handles: Vec<ElementHandle>,
    session: &std::sync::Arc<dyn crate::transport::Session>,
) -> Result<Vec<ElementHandle>> {
    if handles.len() <= 1 {
        return Ok(handles);
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        let node = session
            .send(&agito_protocol::cdp::dom::DescribeNodeParams {
                object_id: Some(h.object_id().to_owned()),
                ..Default::default()
            })
            .await?;
        if seen.insert(node.node.backend_node_id) {
            out.push(h);
        }
    }
    Ok(out)
}

/// Errors an actionability or assertion loop retries rather than surfacing.
pub(crate) fn is_retryable(error: &Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::ElementNotFound { .. }
            | ErrorKind::Evaluation { .. }
            | ErrorKind::Protocol { .. }
            | ErrorKind::CommandTimeout { .. }
    )
}

/// Splits leading `..` / `../` steps from a selector.
pub(crate) fn parse_parent_prefix(selector: &str) -> (u32, &str) {
    let mut rest = selector.trim();
    let mut steps = 0;
    loop {
        if rest == ".." {
            return (steps + 1, "");
        }
        match rest.strip_prefix("../") {
            Some(r) => {
                steps += 1;
                rest = r;
            }
            None => break,
        }
    }
    if steps == 0 {
        (0, selector)
    } else {
        (steps, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_prefixes() {
        assert_eq!(parse_parent_prefix(".."), (1, ""));
        assert_eq!(parse_parent_prefix("../.."), (2, ""));
        assert_eq!(parse_parent_prefix("../button"), (1, "button"));
        assert_eq!(parse_parent_prefix("button"), (0, "button"));
        assert_eq!(parse_parent_prefix("..foo"), (0, "..foo"));
    }
}

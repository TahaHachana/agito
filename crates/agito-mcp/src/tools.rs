//! The tools an agent calls. Each acts on the active tab, addresses elements by a ref from the
//! latest snapshot or by a selector, and reports failure as an error result with guidance the
//! agent can act on, never as a protocol error, which would lose the guidance.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agito::accessibility::{AuditOptions, Severity};
use agito::{ClickOptions, ElementState, Locator, MouseButton, ScreenshotOptions};
use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::caps::Capability;
use crate::runner::{self, Guarded, Report, Work, describe};
use crate::session::{Session, SnapshotError, TargetError, frame_order};
use crate::tab::{Slice, Tab, dialog_pending};
use crate::{DEFAULT_SETTLE, ServerOptions};

/// How every tool describes the element it acts on.
const TARGET: &str = "A ref from the latest snapshot (e5, or f1e5 for an element inside frame 1), \
    or a selector: CSS by default, or prefixed with xpath=, text=, role=, or data-testid=. A \
    selector needs no snapshot.";

const SNAPSHOT_AFTER: &str = "Append a snapshot of the page after the action.";

const SINCE: &str =
    "Return only entries from this sequence number onwards. Omit to read everything the log holds.";

/// A tool each group adds, to tell whether the group is in the catalog already.
fn group_marker(capability: Capability) -> &'static str {
    match capability {
        Capability::Coordinates => "click_xy",
        Capability::Contexts => "context_create",
        Capability::Routing => "route_fulfill",
    }
}

/// The longest response body `network_request` shows.
const BODY_LIMIT: usize = 4000;

pub(crate) fn text(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message.into())])
}

pub(crate) fn error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// The first text block of a result.
fn first_text(result: &CallToolResult) -> &str {
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map_or("", |t| t.text.as_str())
}

/// Refuses a `file:` URL unless the server allows it: a page could otherwise talk an agent
/// into reading the machine's files back through a snapshot.
pub(crate) fn refuse_url(options: &ServerOptions, url: &str) -> Option<String> {
    let scheme = url.split(':').next().unwrap_or_default();
    (!options.allow_unrestricted_file_access && scheme.eq_ignore_ascii_case("file")).then(|| {
        format!(
            "Refused to open '{url}': file: URLs are blocked. Start the server with \
             --allow-unrestricted-file-access to allow them."
        )
    })
}

/// A log read as text: a note when entries dropped off, the entries, and `next=N`.
fn render_log<T>(slice: Slice<T>, line: impl Fn(u64, &T) -> String, nothing: String) -> String {
    let mut out = String::new();
    if slice.dropped > 0 {
        out.push_str(&format!(
            "{} earlier entries are no longer in the log.\n",
            slice.dropped
        ));
    }
    if slice.entries.is_empty() {
        out.push_str(&nothing);
        out.push('\n');
    }
    for (seq, entry) in &slice.entries {
        out.push_str(&line(*seq, entry));
        out.push('\n');
    }
    out.push_str(&format!("next={}", slice.next));
    out
}

/// The `agito mcp` server: one browser session, and the tools that drive it.
#[derive(Clone)]
pub struct AgitoServer {
    pub(crate) options: Arc<ServerOptions>,
    pub(crate) session: Arc<Mutex<Session>>,
    /// Held for the whole of each tool call, so calls run one at a time. MCP lets a client
    /// send calls without waiting, and agents do issue several at once; run together, a click
    /// could land before the snapshot naming its ref.
    calls: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

impl AgitoServer {
    /// A server that starts its browser, with `options`, on the first call that needs one.
    /// It lists the always-available tools, and the groups `options.capabilities` names.
    pub fn new(options: Arc<ServerOptions>) -> Self {
        let mut tool_router = Self::tool_router();
        for capability in &options.capabilities {
            if tool_router.has_route(group_marker(*capability)) {
                continue;
            }
            tool_router += match capability {
                Capability::Coordinates => Self::coordinate_tools(),
                Capability::Contexts => Self::context_tools(),
                Capability::Routing => Self::routing_tools(),
            };
        }
        AgitoServer {
            session: Arc::new(Mutex::new(Session::new(options.clone()))),
            calls: Arc::new(Mutex::new(())),
            options,
            tool_router,
        }
    }

    pub(crate) fn session(&self) -> Arc<Mutex<Session>> {
        self.session.clone()
    }

    fn settle(&self) -> Duration {
        self.options.settle.unwrap_or(DEFAULT_SETTLE)
    }

    /// The active tab, opened if there is none.
    pub(crate) async fn tab(&self) -> Result<Tab, CallToolResult> {
        self.session
            .lock()
            .await
            .active_tab()
            .await
            .map_err(|e| error(format!("Could not open a page: {e}")))
    }

    /// The active tab and a locator for `target` on it. A selector is searched in the
    /// selected frame; a ref keeps addressing the document it was read from.
    pub(crate) async fn target(&self, target: &str) -> Result<(Tab, Locator), CallToolResult> {
        if target.trim().is_empty() {
            return Err(error(
                "ref is required: pass a ref from the latest snapshot or a selector.",
            ));
        }
        let tab = self.tab().await?;
        let frame = tab.state.frame();
        let locator = tab
            .state
            .snapshot
            .lock()
            .await
            .resolve(&tab.page, target, frame.as_ref())
            .map_err(|e| error(e.message()))?;
        Ok((tab, locator))
    }

    /// Takes a snapshot of the tab, of its selected frame when one is, and renders it.
    async fn snapshot_text(
        &self,
        tab: &Tab,
        root_ref: Option<&str>,
        max_depth: Option<usize>,
        max_frames: Option<usize>,
    ) -> Result<String, String> {
        let frame = tab.state.frame();
        let mut state = tab.state.snapshot.lock().await;
        match state
            .take(&tab.page, frame, root_ref, max_depth, max_frames)
            .await
        {
            // Said on every scoped snapshot, since frame_select is often several calls back.
            Ok((text, Some(frame))) => Ok(format!(
                "Scoped to frame {}\n\n{}",
                frame.url(),
                self.next_step(text)
            )),
            Ok((text, None)) => Ok(self.next_step(text)),
            Err(SnapshotError::Target(TargetError::Stale(r))) => Err(format!(
                "Ref '{r}' is not in the latest snapshot. Take a full snapshot to refresh refs."
            )),
            Err(SnapshotError::Target(_)) => {
                Err("No snapshot has been taken. Call snapshot without root_ref first.".into())
            }
            Err(SnapshotError::Browser(e)) => Err(format!("Snapshot failed: {e}")),
        }
    }

    /// Finishes a snapshot that found nothing addressable with the way forward this server
    /// offers: what to do next depends on whether the coordinate tools are in the catalog.
    fn next_step(&self, text: String) -> String {
        let Some(at) = text.find(crate::session::NOTHING_ADDRESSABLE) else {
            return text;
        };
        let coordinates = self.options.capabilities.contains(&Capability::Coordinates);
        let next = if coordinates {
            ", then act on their positions with the coordinate tools: click_xy, drag, or \
             scroll_xy."
        } else {
            ". This server has no tools that act on a position, so nothing on this page can be \
             clicked until it is restarted with --caps coordinates."
        };
        let end = at + crate::session::NOTHING_ADDRESSABLE.len();
        format!("{}{next}{}", &text[..end], &text[end + 1..])
    }

    /// Runs `work`, which reads the page the way snapshot or evaluate do: refused while a
    /// dialog is open, and cut short if one opens.
    async fn read<F>(&self, tab: &Tab, work: F) -> CallToolResult
    where
        F: Future<Output = Work> + Send + 'static,
    {
        match runner::guarded(tab, work).await {
            Err(pending) => error(pending),
            Ok(Guarded::Interrupted(notice)) => text(notice),
            Ok(Guarded::Done(Ok(message))) => text(message),
            Ok(Guarded::Done(Err(message))) => error(message),
        }
    }

    /// Runs an action: guarded against dialogs, then reported (what changed), then followed
    /// by a snapshot when one was asked for.
    pub(crate) async fn act<F>(&self, tab: Tab, snapshot: Option<bool>, work: F) -> CallToolResult
    where
        F: Future<Output = Work> + Send + 'static,
    {
        let report = Report::begin(&self.session, &tab, self.settle()).await;
        let message = match runner::guarded(&tab, work).await {
            Err(pending) => return error(pending),
            Ok(Guarded::Interrupted(notice)) => return text(notice),
            Ok(Guarded::Done(Err(message))) => return error(message),
            Ok(Guarded::Done(Ok(message))) => message,
        };
        let mut out = message.clone();
        for row in report.rows(&message, &self.session, &tab).await {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&row);
        }
        if snapshot == Some(true) {
            out.push_str("\n\n");
            if tab.state.pending_dialog().is_some() {
                out.push_str("Snapshot not taken: the page is waiting on a dialog.");
            } else {
                match self.snapshot_text(&tab, None, None, None).await {
                    Ok(tree) => out.push_str(&format!("Snapshot:\n{tree}")),
                    Err(e) => out.push_str(&format!("Snapshot not taken: {e}")),
                }
            }
        }
        text(out)
    }

    /// Runs an action on the element `target` names and reports `done` when it succeeds.
    async fn on_element<F, Fut>(
        &self,
        target: &str,
        done: String,
        snapshot: Option<bool>,
        action: F,
    ) -> CallToolResult
    where
        F: FnOnce(Locator) -> Fut + Send + 'static,
        Fut: Future<Output = agito::Result<()>> + Send + 'static,
    {
        let (tab, locator) = match self.target(target).await {
            Ok(found) => found,
            Err(result) => return result,
        };
        let target = target.to_owned();
        self.act(tab, snapshot, async move {
            action(locator)
                .await
                .map(|()| done)
                .map_err(|e| format!("{target}: {e}"))
        })
        .await
    }

    /// Runs a navigation, forgets the refs it retired, and reports where it went: `go`
    /// returns the verb for the result's line, or `None` when there was nowhere to go and
    /// `nowhere` says so instead.
    async fn navigation<F, Fut>(
        &self,
        snapshot: Option<bool>,
        failed: &'static str,
        nowhere: &'static str,
        go: F,
    ) -> CallToolResult
    where
        F: FnOnce(Tab) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Option<&'static str>, agito::Error>> + Send + 'static,
    {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let moving = tab.clone();
        self.act(tab, snapshot, async move {
            let verb = go(moving.clone())
                .await
                .map_err(|e| format!("{failed}: {e}"))?;
            let Some(verb) = verb else {
                return Ok(nowhere.to_owned());
            };
            moving.state.snapshot.lock().await.retire();
            moving.state.select_frame(None);
            Ok(format!("{verb} {}", describe(&moving).await))
        })
        .await
    }
}

/// `navigate`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct NavigateArgs {
    /// The URL to navigate to.
    url: String,
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// Arguments for the tools that take only `snapshot`.
#[derive(Deserialize, JsonSchema)]
pub struct SnapshotAfterArgs {
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// `snapshot`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct SnapshotArgs {
    /// Root the snapshot at the subtree of this ref from the previous snapshot. A ref inside a
    /// frame (f1e2) roots it in that frame.
    root_ref: Option<String>,
    /// Limit how many levels deep the tree is rendered; 0 renders only the root. A frame's
    /// contents count as levels of the tree like anything else.
    max_depth: Option<usize>,
    /// Limit how many frames are printed inside the page; 10 by default. Frames past the limit
    /// are named by frame_list and read with frame_select.
    max_frames: Option<usize>,
}

/// `screenshot`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct ScreenshotArgs {
    /// Capture the full scrollable page instead of just the viewport.
    full_page: Option<bool>,
}

/// `click`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct ClickArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
    /// Double-click instead of a single click.
    double: Option<bool>,
    /// The mouse button: left (default), right, or middle.
    button: Option<String>,
    /// Modifier keys held during the click: Alt, Control, Meta, Shift.
    modifiers: Option<Vec<String>>,
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// `type`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TypeArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
    /// The text to enter.
    text: String,
    /// Press Enter after entering the text.
    submit: Option<bool>,
    /// Type character by character instead of setting the value at once.
    slowly: Option<bool>,
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// Arguments for the tools that take only an element.
#[derive(Deserialize, JsonSchema)]
pub struct ElementArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
}

/// `press`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct PressArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
    /// The key to press, e.g. Enter, Tab, Escape, ArrowDown, or a single character.
    key: String,
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// `press_key`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct PressKeyArgs {
    /// The key to press, e.g. Escape, Tab, Enter, ArrowDown, or a single character.
    key: String,
}

/// `select_option`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct SelectOptionArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
    /// The option values to select.
    values: Vec<String>,
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// `set_checked`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct SetCheckedArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
    /// The desired state: true to check, false to uncheck.
    checked: bool,
    #[schemars(description = SNAPSHOT_AFTER)]
    snapshot: Option<bool>,
}

/// `wait_for_element`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct WaitForElementArgs {
    #[schemars(description = TARGET)]
    r#ref: String,
    /// The state to wait for: visible, hidden, attached, or detached.
    state: String,
}

/// `wait_for`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct WaitForArgs {
    /// Time to wait in milliseconds.
    time: Option<u64>,
    /// Wait until this text appears anywhere on the page or the scoped frame.
    text: Option<String>,
    /// Wait until this text is gone from the page or the scoped frame.
    text_gone: Option<String>,
}

/// `evaluate`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct EvaluateArgs {
    /// The JavaScript expression to evaluate.
    expression: String,
    /// The element to evaluate against, passed to the function as its argument and as `this`.
    /// Omit to evaluate in the page, or in the scoped frame. A ref from the latest snapshot or
    /// a selector.
    r#ref: Option<String>,
}

/// `tab_open`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TabOpenArgs {
    /// URL to open the new tab at. Omit to open a blank tab.
    url: Option<String>,
}

/// Arguments for the tools that take a tab or frame index.
#[derive(Deserialize, JsonSchema)]
pub struct IndexArgs {
    /// Zero-based index, from tab_list or frame_list.
    index: usize,
}

/// `tab_close`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TabCloseArgs {
    /// Zero-based index of the tab to close. Omit to close the active tab.
    index: Option<usize>,
}

/// `handle_dialog`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct HandleDialogArgs {
    /// True to accept the dialog, false to dismiss it.
    accept: bool,
    /// Text to enter in a prompt dialog when accepting. Ignored for alert and confirm.
    text: Option<String>,
}

/// Arguments for the log readers.
#[derive(Deserialize, JsonSchema)]
pub struct SinceArgs {
    #[schemars(description = SINCE)]
    since: Option<u64>,
}

/// `network_request`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct NetworkRequestArgs {
    /// The sequence number of the request, as shown in square brackets by network_requests.
    sequence: u64,
}

/// `audit_accessibility`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct AuditArgs {
    /// Only return violations at or above this severity: error, warning, or info. Omit to
    /// return all severities.
    min_severity: Option<String>,
}

pub(crate) fn parse_button(button: Option<&str>) -> Result<MouseButton, CallToolResult> {
    match button.map(str::to_ascii_lowercase).as_deref() {
        None | Some("left") => Ok(MouseButton::Left),
        Some("right") => Ok(MouseButton::Right),
        Some("middle") => Ok(MouseButton::Middle),
        Some(other) => Err(error(format!(
            "Unknown button '{other}'. Use one of: left, right, middle."
        ))),
    }
}

pub(crate) fn parse_modifiers(modifiers: &[String]) -> Result<u32, CallToolResult> {
    modifiers.iter().try_fold(0, |bits, m| {
        let bit = match m.to_ascii_lowercase().as_str() {
            "alt" => 1,
            "control" | "ctrl" => 2,
            "meta" | "command" | "cmd" => 4,
            "shift" => 8,
            _ => {
                return Err(error(format!(
                    "Unknown modifier '{m}'. Use any of: Alt, Control, Meta, Shift."
                )));
            }
        };
        Ok(bits | bit)
    })
}

/// Calls the function `expression` evaluates to with the element as its argument and `this`.
fn element_function(expression: &str) -> String {
    format!(
        "async function() {{ const f = ({expression}\n); if (typeof f !== 'function') throw new \
         Error('Given expression does not evaluate to a function'); return await f.call(this, this); }}"
    )
}

/// Whether a content type is worth showing as text.
fn is_text(content_type: &str) -> bool {
    let c = content_type.to_ascii_lowercase();
    c.starts_with("text/")
        || ["json", "xml", "javascript", "x-www-form-urlencoded"]
            .iter()
            .any(|t| c.contains(t))
}

fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
}

#[tool_router]
impl AgitoServer {
    #[tool(
        name = "navigate",
        description = "Navigates the active page to a URL, waits for it to load, and reports the \
            page it ended up on with its title. Refs from any earlier snapshot stop meaning \
            anything here, so take a snapshot before addressing elements, or pass snapshot: true \
            to get one with the result.",
        annotations(title = "Navigate to URL", destructive_hint = true)
    )]
    async fn navigate(&self, Parameters(args): Parameters<NavigateArgs>) -> CallToolResult {
        if args.url.trim().is_empty() {
            return error("url is required.");
        }
        if let Some(refusal) = refuse_url(&self.options, &args.url) {
            return error(refusal);
        }
        let url = args.url;
        self.navigation(
            args.snapshot,
            "Navigation failed",
            "",
            async move |tab: Tab| {
                tab.page.goto(&url).await?;
                Ok(Some("Navigated to"))
            },
        )
        .await
    }

    #[tool(
        name = "go_back",
        description = "Navigates the active tab back one entry in its history. Reports when there \
            was no entry to go to.",
        annotations(title = "Go back", destructive_hint = false)
    )]
    async fn go_back(&self, Parameters(args): Parameters<SnapshotAfterArgs>) -> CallToolResult {
        self.navigation(
            args.snapshot,
            "Go back failed",
            "No previous history entry; the page did not change.",
            async |tab: Tab| Ok(tab.page.go_back().await?.map(|_| "Navigated back to")),
        )
        .await
    }

    #[tool(
        name = "go_forward",
        description = "Navigates the active tab forward one entry in its history. Reports when \
            there was no entry to go to.",
        annotations(title = "Go forward", destructive_hint = false)
    )]
    async fn go_forward(&self, Parameters(args): Parameters<SnapshotAfterArgs>) -> CallToolResult {
        self.navigation(
            args.snapshot,
            "Go forward failed",
            "No next history entry; the page did not change.",
            async |tab: Tab| Ok(tab.page.go_forward().await?.map(|_| "Navigated forward to")),
        )
        .await
    }

    #[tool(
        name = "reload",
        description = "Reloads the active tab and waits for it to finish loading.",
        annotations(title = "Reload", destructive_hint = false)
    )]
    async fn reload(&self, Parameters(args): Parameters<SnapshotAfterArgs>) -> CallToolResult {
        self.navigation(args.snapshot, "Reload failed", "", async |tab: Tab| {
            tab.page.reload().await?;
            Ok(Some("Reloaded"))
        })
        .await
    }

    #[tool(
        name = "snapshot",
        description = "Returns a compact indented accessibility tree of the active page, or of \
            the scoped frame when one is selected. Interactive, named, and focusable elements and \
            iframes carry a ref (e1, e2, ...) that click and type use to address them, and those \
            tools take a selector just as readily; other nodes are printed for context without a \
            ref. Text is printed inline on its element's line, headings show [level=N], links \
            show [url=...], and form controls show [value=\"...\"] and state flags. A page tree \
            contains the frames the page hosts, printed inside the iframe element that holds \
            each and marked [frame=N]; refs inside frame 1 read f1e2 and act on that frame \
            without selecting it first.",
        annotations(
            title = "Accessibility snapshot",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn snapshot(&self, Parameters(args): Parameters<SnapshotArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let server = self.clone();
        let reading = tab.clone();
        self.read(&tab, async move {
            server
                .snapshot_text(
                    &reading,
                    args.root_ref.as_deref(),
                    args.max_depth,
                    args.max_frames,
                )
                .await
        })
        .await
    }

    #[tool(
        name = "screenshot",
        description = "Captures a PNG screenshot of the active page and returns it as an image.",
        annotations(
            title = "Screenshot",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn screenshot(&self, Parameters(args): Parameters<ScreenshotArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        if let Some(dialog) = tab.state.pending_dialog() {
            return error(dialog_pending(&dialog));
        }
        let options = ScreenshotOptions {
            full_page: args.full_page.unwrap_or(false),
            ..Default::default()
        };
        match tab.page.screenshot(options).await {
            Ok(bytes) => CallToolResult::success(vec![ContentBlock::image(
                base64::engine::general_purpose::STANDARD.encode(bytes),
                "image/png",
            )]),
            Err(e) => error(format!("Screenshot failed: {e}")),
        }
    }

    #[tool(
        name = "click",
        description = "Clicks the element addressed by a ref from the latest snapshot or by a \
            selector. A right-click, a middle-click, and a click with modifier keys held all go \
            through the same actionability checks as a plain one. The result names anything the \
            click changed: where the page went, a tab it opened, errors it logged, a dialog it \
            raised.",
        annotations(title = "Click element", destructive_hint = true)
    )]
    async fn click(&self, Parameters(args): Parameters<ClickArgs>) -> CallToolResult {
        let button = match parse_button(args.button.as_deref()) {
            Ok(button) => button,
            Err(result) => return result,
        };
        let modifiers = match parse_modifiers(args.modifiers.as_deref().unwrap_or_default()) {
            Ok(bits) => bits,
            Err(result) => return result,
        };
        let double = args.double == Some(true);
        if double && (button != MouseButton::Left || modifiers != 0) {
            return error(
                "A double-click uses the left button with no modifiers. Drop double, or click \
                 twice with the button and modifiers you need.",
            );
        }
        let done = format!("Clicked {}", args.r#ref);
        self.on_element(&args.r#ref, done, args.snapshot, async move |locator| {
            if double {
                locator.dblclick().await
            } else {
                locator
                    .click_with(ClickOptions {
                        button,
                        modifiers,
                        ..Default::default()
                    })
                    .await
            }
        })
        .await
    }

    #[tool(
        name = "type",
        description = "Types text into the element addressed by a ref from the latest snapshot or \
            by a selector.",
        annotations(title = "Type into element", destructive_hint = true)
    )]
    async fn type_text(&self, Parameters(args): Parameters<TypeArgs>) -> CallToolResult {
        let done = format!("Typed into {}", args.r#ref);
        let slowly = args.slowly == Some(true);
        let submit = args.submit == Some(true);
        let typed = args.text;
        self.on_element(&args.r#ref, done, args.snapshot, async move |locator| {
            if slowly {
                locator.type_text(&typed).await?;
            } else {
                locator.fill(&typed).await?;
            }
            if submit {
                locator.press("Enter").await?;
            }
            Ok(())
        })
        .await
    }

    #[tool(
        name = "press",
        description = "Presses a key while the element addressed by a ref or a selector is \
            focused, e.g. Enter, Tab, ArrowDown.",
        annotations(title = "Press a key on element", destructive_hint = true)
    )]
    async fn press(&self, Parameters(args): Parameters<PressArgs>) -> CallToolResult {
        if args.key.is_empty() {
            return error("key is required.");
        }
        let done = format!("Pressed {} on {}", args.key, args.r#ref);
        let key = args.key;
        self.on_element(&args.r#ref, done, args.snapshot, async move |locator| {
            locator.press(&key).await
        })
        .await
    }

    #[tool(
        name = "press_key",
        description = "Presses a key on the active page without targeting an element, e.g. \
            Escape, Tab, Enter. No snapshot is required.",
        annotations(title = "Press a key", destructive_hint = true)
    )]
    async fn press_key(&self, Parameters(args): Parameters<PressKeyArgs>) -> CallToolResult {
        if args.key.is_empty() {
            return error("key is required.");
        }
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let page = tab.page.clone();
        self.act(tab, None, async move {
            page.keyboard()
                .press(&args.key)
                .await
                .map(|()| format!("Pressed {}", args.key))
                .map_err(|e| format!("Press key failed: {e}"))
        })
        .await
    }

    #[tool(
        name = "hover",
        description = "Moves the pointer over the element addressed by a ref from the latest \
            snapshot or by a selector.",
        annotations(title = "Hover over element", destructive_hint = true)
    )]
    async fn hover(&self, Parameters(args): Parameters<ElementArgs>) -> CallToolResult {
        let done = format!("Hovered {}", args.r#ref);
        self.on_element(&args.r#ref, done, None, async |l| l.hover().await)
            .await
    }

    #[tool(
        name = "focus",
        description = "Focuses the element addressed by a ref from the latest snapshot or by a \
            selector.",
        annotations(title = "Focus element", destructive_hint = true)
    )]
    async fn focus(&self, Parameters(args): Parameters<ElementArgs>) -> CallToolResult {
        let done = format!("Focused {}", args.r#ref);
        self.on_element(&args.r#ref, done, None, async |l| l.focus().await)
            .await
    }

    #[tool(
        name = "clear",
        description = "Clears the value of the input or textarea addressed by a ref from the \
            latest snapshot or by a selector.",
        annotations(title = "Clear input", destructive_hint = true)
    )]
    async fn clear(&self, Parameters(args): Parameters<ElementArgs>) -> CallToolResult {
        let done = format!("Cleared {}", args.r#ref);
        self.on_element(&args.r#ref, done, None, async |l| l.clear().await)
            .await
    }

    #[tool(
        name = "scroll_into_view",
        description = "Scrolls the element addressed by a ref or a selector into the viewport if \
            it is not already visible.",
        annotations(title = "Scroll element into view", destructive_hint = true)
    )]
    async fn scroll_into_view(&self, Parameters(args): Parameters<ElementArgs>) -> CallToolResult {
        let done = format!("Scrolled {} into view", args.r#ref);
        self.on_element(&args.r#ref, done, None, async |l| {
            l.scroll_into_view().await
        })
        .await
    }

    #[tool(
        name = "select_option",
        description = "Selects one or more options in a <select> element addressed by a ref or a \
            selector, by their values.",
        annotations(title = "Select dropdown options", destructive_hint = true)
    )]
    async fn select_option(
        &self,
        Parameters(args): Parameters<SelectOptionArgs>,
    ) -> CallToolResult {
        if args.values.is_empty() {
            return error("values is required: name at least one option value.");
        }
        let done = format!("Selected {} option(s) in {}", args.values.len(), args.r#ref);
        let values = args.values;
        self.on_element(&args.r#ref, done, args.snapshot, async move |locator| {
            let values: Vec<&str> = values.iter().map(String::as_str).collect();
            locator.select_option(&values).await.map(drop)
        })
        .await
    }

    #[tool(
        name = "set_checked",
        description = "Sets the checked state of a checkbox or radio button addressed by a ref or \
            a selector. Clicks only when the state differs, and reports an error if the click left \
            it unchanged.",
        annotations(title = "Set checkbox state", destructive_hint = true)
    )]
    async fn set_checked(&self, Parameters(args): Parameters<SetCheckedArgs>) -> CallToolResult {
        let done = format!("Set {} checked={}", args.r#ref, args.checked);
        let checked = args.checked;
        self.on_element(&args.r#ref, done, args.snapshot, async move |locator| {
            locator.set_checked(checked).await
        })
        .await
    }

    #[tool(
        name = "wait_for_element",
        description = "Waits until the element addressed by a ref or a selector reaches a state: \
            visible, hidden, attached, or detached.",
        annotations(
            title = "Wait for element state",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn wait_for_element(
        &self,
        Parameters(args): Parameters<WaitForElementArgs>,
    ) -> CallToolResult {
        let state = match args.state.to_ascii_lowercase().as_str() {
            "visible" => ElementState::Visible,
            "hidden" => ElementState::Hidden,
            "attached" => ElementState::Attached,
            "detached" => ElementState::Detached,
            other => {
                return error(format!(
                    "Unknown state '{other}'. Use one of: visible, hidden, attached, detached."
                ));
            }
        };
        let (tab, locator) = match self.target(&args.r#ref).await {
            Ok(found) => found,
            Err(result) => return result,
        };
        let done = format!(
            "{} reached state {}",
            args.r#ref,
            args.state.to_ascii_lowercase()
        );
        let target = args.r#ref;
        self.read(&tab, async move {
            locator
                .wait_for(state, None)
                .await
                .map(|()| done)
                .map_err(|e| format!("{target}: {e}"))
        })
        .await
    }

    #[tool(
        name = "wait_for",
        description = "Waits for a page condition: a fixed time, text to appear, or text to \
            disappear. The text waits look at the scoped frame when one is selected, and at the \
            page otherwise. Provide exactly one of time, text, or text_gone.",
        annotations(
            title = "Wait for a page condition",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn wait_for(&self, Parameters(args): Parameters<WaitForArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let (wanted, present) = match (args.time, args.text, args.text_gone) {
            (Some(ms), None, None) => {
                tab.page.wait_for_timeout(Duration::from_millis(ms)).await;
                return text(format!("Waited {ms} ms"));
            }
            (None, Some(t), None) => (t, true),
            (None, None, Some(t)) => (t, false),
            _ => return error("Provide exactly one of time, text, or text_gone."),
        };
        let probe = format!(
            "!!document.body && document.body.innerText.includes({})",
            serde_json::Value::from(wanted.as_str())
        );
        let frame = tab.state.frame().unwrap_or_else(|| tab.page.main_frame());
        let timeout = tab.page.context().options().action_timeout;
        self.read(&tab, async move {
            let deadline = Instant::now() + timeout;
            loop {
                if frame.evaluate::<bool>(&probe).await.ok() == Some(present) {
                    return Ok(if present {
                        format!("Text appeared: {wanted}")
                    } else {
                        format!("Text gone: {wanted}")
                    });
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "Wait failed: timed out after {} ms waiting for '{wanted}' to {}.",
                        timeout.as_millis(),
                        if present { "appear" } else { "disappear" }
                    ));
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
    }

    #[tool(
        name = "evaluate",
        description = "Evaluates a JavaScript expression and returns its result as structured \
            JSON under a \"result\" key, so an expression may return a value of any shape: a \
            number, a string, an array, or an object. With no ref it runs in the page, or in the \
            scoped frame when one is selected, and may be statements, whose value is the last \
            one's; with a ref or a selector the expression must be a \
            function, and runs against that element, passed as its argument. Results that cannot \
            be serialized (undefined, functions, DOM nodes) come back as null.",
        annotations(title = "Evaluate JavaScript", destructive_hint = true)
    )]
    async fn evaluate(&self, Parameters(args): Parameters<EvaluateArgs>) -> CallToolResult {
        if args.expression.trim().is_empty() {
            return error("expression is required.");
        }
        let (tab, locator) = match args.r#ref.as_deref().filter(|r| !r.trim().is_empty()) {
            None => match self.tab().await {
                Ok(tab) => (tab, None),
                Err(result) => return result,
            },
            Some(target) => match self.target(target).await {
                Ok((tab, locator)) => (tab, Some(locator)),
                Err(result) => return result,
            },
        };
        let frame = tab.state.frame().unwrap_or_else(|| tab.page.main_frame());
        let expression = args.expression;
        let result = self
            .read(&tab, async move {
                let value = match locator {
                    None => {
                        let value = frame
                            .evaluate::<Option<serde_json::Value>>(&expression)
                            .await;
                        match value {
                            // Statements, such as `a = 1; b = 2`, are not an expression: run
                            // them as a script, whose value is the last statement's.
                            Err(e) if e.to_string().contains("SyntaxError") => {
                                let script = serde_json::Value::from(expression.as_str());
                                frame
                                    .evaluate::<Option<serde_json::Value>>(&format!(
                                        "(0, eval)({script})"
                                    ))
                                    .await
                                    .map_err(|_| e)
                            }
                            other => other,
                        }
                    }
                    Some(locator) => {
                        locator
                            .evaluate::<Option<serde_json::Value>>(
                                &element_function(&expression),
                                &[],
                            )
                            .await
                    }
                };
                // Structured content must be an object, and one shape for every value keeps a
                // single count as easy to read as a record.
                value
                    .map(|v| json!({ "result": v.unwrap_or_default() }).to_string())
                    .map_err(|e| format!("Evaluate failed: {e}"))
            })
            .await;
        if result.is_error == Some(true) {
            return result;
        }
        match serde_json::from_str::<serde_json::Value>(first_text(&result)) {
            Ok(value) => CallToolResult::structured(value),
            // A dialog the expression opened.
            Err(_) => result,
        }
    }

    #[tool(
        name = "handle_dialog",
        description = "Accepts or dismisses the open JavaScript dialog (alert, confirm, or \
            prompt). A dialog blocks the page until it is handled. Returns an error when no \
            dialog is open. For a prompt, text is the value to enter when accepting.",
        annotations(title = "Handle a dialog", destructive_hint = true)
    )]
    async fn handle_dialog(
        &self,
        Parameters(args): Parameters<HandleDialogArgs>,
    ) -> CallToolResult {
        let tab = self.session.lock().await.current_tab();
        let Some(dialog) = tab.and_then(|t| t.state.take_dialog()) else {
            return error("No dialog is open.");
        };
        let outcome = if args.accept {
            dialog.accept(args.text.as_deref()).await
        } else {
            dialog.dismiss().await
        };
        match outcome {
            Ok(()) => text(format!(
                "Dialog ({}) {}: {}",
                dialog.kind,
                if args.accept { "accepted" } else { "dismissed" },
                dialog.message
            )),
            Err(e) => error(format!("Handling dialog failed: {e}")),
        }
    }

    #[tool(
        name = "console_messages",
        description = "Returns the console messages and uncaught page errors the active tab has \
            logged. Each line is [type] text; an uncaught error has the type pageerror. Reading \
            does not clear the log, so the same call can be made again. The last line is next=N: \
            pass that as since to read only what arrives after this read. An action result that \
            counts errors gives the since value that returns exactly those.",
        annotations(
            title = "Read console output",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn console_messages(&self, Parameters(args): Parameters<SinceArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let slice = tab.state.console.lock().unwrap().read(args.since);
        text(render_log(
            slice,
            |_, entry| entry.to_string(),
            match args.since {
                None => "No console messages have been logged.".into(),
                Some(s) => format!("No console messages have been logged since {s}."),
            },
        ))
    }

    #[tool(
        name = "network_requests",
        description = "Returns the requests the active tab has finished. Each line is [n] METHOD \
            STATUS URL (resource type), where n is the sequence number network_request takes; a \
            failed or blocked request shows FAILED. Reading does not clear the log, so the same \
            call can be made again. The last line is next=N: pass that as since to read only what \
            arrives after this read.",
        annotations(
            title = "Read the request log",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn network_requests(&self, Parameters(args): Parameters<SinceArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let slice = tab.state.network.lock().unwrap().read(args.since);
        text(render_log(
            slice,
            |seq, entry| format!("[{seq}] {entry}"),
            match args.since {
                None => "No requests have been logged.".into(),
                Some(s) => format!("No requests have been logged since {s}."),
            },
        ))
    }

    #[tool(
        name = "network_request",
        description = "Returns the detail the log holds for one request: its method, status, URL, \
            resource type, request and response headers, any request body, and the response body \
            when the browser can still produce it. The sequence number is the [n] that \
            network_requests prints in front of each line.",
        annotations(
            title = "Read one request",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn network_request(
        &self,
        Parameters(args): Parameters<NetworkRequestArgs>,
    ) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let found = tab.state.network.lock().unwrap().find(args.sequence);
        let Some(entry) = found else {
            return error(format!(
                "No request with sequence {} is in the log. Call network_requests to see what it \
                 holds.",
                args.sequence
            ));
        };
        let mut out = format!("[{}] {entry}\n", args.sequence);
        for (label, headers) in [
            ("Request headers", &entry.request_headers),
            ("Response headers", &entry.response_headers),
        ] {
            if !headers.is_empty() {
                out.push_str(&format!("{label}:\n"));
                for (k, v) in headers {
                    out.push_str(&format!("  {k}: {v}\n"));
                }
            }
        }
        if let Some(body) = &entry.post_data {
            out.push_str(&format!("Request body:\n{body}\n"));
        }
        let content_type = entry.response_header("content-type").unwrap_or_default();
        let length: Option<usize> = entry
            .response_header("content-length")
            .and_then(|l| l.trim().parse().ok());
        let body = if entry.status.is_none() {
            "the request failed, so there is none.".to_owned()
        } else if !content_type.is_empty() && !is_text(content_type) {
            format!("not shown, because it is {content_type}.")
        } else if let Some(length) = length.filter(|l| *l > BODY_LIMIT) {
            format!("not shown, because it is {length} bytes.")
        } else {
            match entry.body().await {
                None => "the browser no longer has it. Response data is evicted, and a \
                         navigation drops it at once."
                    .to_owned(),
                Some(body) if body.chars().count() <= BODY_LIMIT => format!("\n{body}"),
                Some(body) => format!(
                    "\n{}... (truncated)",
                    body.chars().take(BODY_LIMIT).collect::<String>()
                ),
            }
        };
        out.push_str(&format!("Response body: {body}"));
        text(out)
    }

    #[tool(
        name = "frame_list",
        description = "Lists the frames of the active page, each with its zero-based index, \
            nesting depth, URL, and name. Index 0 is the page itself and a frame is listed after \
            the frame that holds it, with frames at the same level in the order they attached. \
            The scoped frame is marked with an asterisk. The index is the one a page snapshot \
            prints as [frame=N] and puts in front of the refs inside it.",
        annotations(
            title = "List frames",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn frame_list(&self) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let scoped = tab.state.frame();
        let frames = frame_order(&tab.page);
        let mut lines = Vec::new();
        for (i, (frame, depth)) in frames.iter().enumerate() {
            let marked = match &scoped {
                None => i == 0,
                Some(s) => s == frame,
            };
            let mut line = format!(
                "{}[{i}] {}{}{}",
                if marked { "* " } else { "  " },
                "  ".repeat(*depth),
                if i == 0 { "(page) " } else { "" },
                frame.url()
            );
            let name = frame.name();
            if !name.is_empty() {
                line.push_str(&format!(" | {name}"));
            }
            lines.push(line);
        }
        if frames.len() == 1 {
            lines.push("This page has no frames beyond itself.".into());
        }
        text(lines.join("\n"))
    }

    #[tool(
        name = "frame_select",
        description = "Scopes snapshot, evaluate, selectors and the text waits to the frame at \
            the given zero-based index. Clicking and typing inside a frame need no scope: a page \
            snapshot prints every frame and its refs reach into them. Index 0 returns to the \
            page. Indices come from frame_list. Take a snapshot afterwards: refs from the \
            previous scope do not carry over.",
        annotations(title = "Select a frame", destructive_hint = false)
    )]
    async fn frame_select(&self, Parameters(args): Parameters<IndexArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let frames = frame_order(&tab.page);
        let Some((frame, _)) = frames.get(args.index) else {
            return error(format!(
                "There is no frame {}: the page has {} frame{}, numbered 0 to {}. Call frame_list \
                 to see them.",
                args.index,
                frames.len(),
                if frames.len() == 1 { "" } else { "s" },
                frames.len() - 1
            ));
        };
        if args.index == 0 {
            tab.state.select_frame(None);
            return text(format!("Scoped back to the page: {}", frame.url()));
        }
        tab.state.select_frame(Some(frame.clone()));
        text(format!(
            "Scoped to frame {}: {}. Take a snapshot to address its elements.",
            args.index,
            frame.url()
        ))
    }

    #[tool(
        name = "audit_accessibility",
        description = "Runs WCAG 2.1 A and AA accessibility checks against the active page and \
            its frames and returns the violations found, each with a rule id, severity, message, \
            the node's role, name, and text, a best-effort CSS selector, the frame it is in, and \
            (when the snapshot gave the element a ref) a ref usable with click, type, and other \
            tools. A fresh snapshot is taken as part of the audit, so refs from an earlier \
            snapshot are replaced.",
        annotations(
            title = "Audit accessibility",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn audit_accessibility(&self, Parameters(args): Parameters<AuditArgs>) -> CallToolResult {
        let threshold = match args
            .min_severity
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            None | Some("") => None,
            Some("error") => Some(Severity::Error),
            Some("warning") => Some(Severity::Warning),
            Some("info") => Some(Severity::Info),
            Some(other) => {
                return error(format!(
                    "Unknown severity '{other}'. Use error, warning, or info."
                ));
            }
        };
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let auditing = tab.clone();
        let result = self
            .read(&tab, async move {
                let mut snapshot = auditing.state.snapshot.lock().await;
                snapshot
                    .take(&auditing.page, None, None, None, None)
                    .await
                    .map_err(|_| "Accessibility audit failed: the page could not be read.")?;
                let audit = auditing
                    .page
                    .audit_accessibility(&AuditOptions::default())
                    .await
                    .map_err(|e| format!("Accessibility audit failed: {e}"))?;
                let page_url = auditing.page.url();
                let violations: Vec<serde_json::Value> = audit
                    .violations
                    .iter()
                    .filter(|v| {
                        threshold.is_none_or(|t| severity_rank(v.severity) <= severity_rank(t))
                    })
                    .map(|v| {
                        let node = v.backend_node_id;
                        json!({
                            "ruleId": v.rule_id,
                            "severity": v.severity,
                            "message": v.message,
                            "nodeRole": v.role,
                            "nodeName": v.name,
                            "nodeText": node
                                .filter(|_| v.frame_url == page_url)
                                .and_then(|n| snapshot.text_for(n)),
                            "selector": v.selector,
                            "frameUrl": (v.frame_url != page_url).then_some(&v.frame_url),
                            "ref": node.and_then(|n| snapshot.ref_for(&v.frame_url, &page_url, n)),
                        })
                    })
                    .collect();
                Ok(
                    json!({ "violationCount": violations.len(), "violations": violations })
                        .to_string(),
                )
            })
            .await;
        if result.is_error == Some(true) {
            return result;
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(first_text(&result)) else {
            return result;
        };
        let count = payload["violationCount"].as_u64().unwrap_or(0);
        let mut summary = match (count, threshold) {
            (0, None) => "No accessibility violations found.".to_owned(),
            (0, Some(t)) => format!("No accessibility violations found at or above {t} severity."),
            (n, _) => format!("Found {n} accessibility violation(s):"),
        };
        for v in payload["violations"].as_array().into_iter().flatten() {
            let at = v["ref"]
                .as_str()
                .or(v["selector"].as_str())
                .map(|a| format!(" ({a})"))
                .unwrap_or_default();
            summary.push_str(&format!(
                "\n[{}] {}: {}{at}",
                v["severity"].as_str().unwrap_or_default(),
                v["ruleId"].as_str().unwrap_or_default(),
                v["message"].as_str().unwrap_or_default()
            ));
        }
        let mut structured = CallToolResult::structured(payload);
        structured.content = vec![ContentBlock::text(summary)];
        structured
    }

    #[tool(
        name = "tab_list",
        description = "Lists every open tab this session has, each with its zero-based index, \
            URL, and title. The index is the one tab_select and tab_close take.",
        annotations(
            title = "List tabs",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn tab_list(&self) -> CallToolResult {
        let (tabs, named) = {
            let session = self.session.lock().await;
            (session.tabs(), session.context_names().len() > 1)
        };
        if tabs.is_empty() {
            return text("No tabs are open.");
        }
        let mut lines = Vec::new();
        for (i, (context, tab)) in tabs.iter().enumerate() {
            let mut line = format!("[{i}] {}", describe(tab).await);
            if named {
                line.push_str(&format!(" | context: {context}"));
            }
            lines.push(line);
        }
        text(lines.join("\n"))
    }

    #[tool(
        name = "tab_open",
        description = "Opens a new tab and makes it active. Navigates it to the URL when one is \
            given. Take a snapshot before addressing elements in the new tab.",
        annotations(title = "Open a tab", destructive_hint = true)
    )]
    async fn tab_open(&self, Parameters(args): Parameters<TabOpenArgs>) -> CallToolResult {
        let url = args.url.filter(|u| !u.trim().is_empty());
        if let Some(refusal) = url.as_deref().and_then(|u| refuse_url(&self.options, u)) {
            return error(refusal);
        }
        let tab = match self.session.lock().await.open_tab().await {
            Ok(tab) => tab,
            Err(e) => return error(format!("Opening tab failed: {e}")),
        };
        if let Some(url) = &url
            && let Err(e) = tab.page.goto(url).await
        {
            return error(format!("Opening tab failed: {e}"));
        }
        text(format!(
            "Opened tab at {}",
            url.as_deref().unwrap_or("about:blank")
        ))
    }

    #[tool(
        name = "tab_select",
        description = "Makes the tab at the given zero-based index active. Indices come from \
            tab_list; call it first if you are unsure of the current order.",
        annotations(title = "Select a tab", destructive_hint = false)
    )]
    async fn tab_select(&self, Parameters(args): Parameters<IndexArgs>) -> CallToolResult {
        let selected = self.session.lock().await.select_tab(args.index);
        match selected {
            Ok(tab) => text(format!("Selected tab {}: {}", args.index, tab.page.url())),
            Err(message) => error(message),
        }
    }

    #[tool(
        name = "tab_close",
        description = "Closes the tab at the given zero-based index, or the active tab when no \
            index is given. The next available tab becomes active.",
        annotations(title = "Close a tab", destructive_hint = true)
    )]
    async fn tab_close(&self, Parameters(args): Parameters<TabCloseArgs>) -> CallToolResult {
        match self.session.lock().await.close_tab(args.index).await {
            Ok(index) => text(format!("Closed tab {index}.")),
            Err(message) => error(message),
        }
    }

    #[tool(
        name = "browser_status",
        description = "Reports which browser the session is driving: whether one has started, \
            and how many contexts and tabs are open.",
        annotations(
            title = "Browser status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn browser_status(&self) -> CallToolResult {
        let session = self.session.lock().await;
        if !session.is_browser_launched() {
            return text("No browser yet. The first tool call that needs one will start it.");
        }
        let contexts = session.context_names().len();
        let tabs = session.tabs().len();
        text(format!(
            "Driving a browser started by this server; it will be closed when this session \
             ends.\n{contexts} context{} (active: {}), {tabs} tab{} open.",
            if contexts == 1 { "" } else { "s" },
            session.active_context_name(),
            if tabs == 1 { "" } else { "s" },
        ))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AgitoServer {
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        // tokio's mutex is fair: waiting calls take their turn in the order they asked.
        let _turn = self.calls.lock().await;
        let call = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let mut response = self.tool_router.call(call).await?;
        // A dialog left open blocks everything after it, so every result says so until it is
        // answered, unless the result already does.
        let pending = self
            .session
            .lock()
            .await
            .current_tab()
            .and_then(|t| t.state.pending_dialog());
        if let (Some(dialog), rmcp::model::CallToolResponse::Complete(result)) =
            (pending, &mut response)
        {
            let said = result.content.iter().any(|c| {
                c.as_text().is_some_and(|t| {
                    t.text.contains("handle_dialog") && t.text.contains(&dialog.message)
                })
            });
            if !said {
                result
                    .content
                    .insert(0, ContentBlock::text(dialog_pending(&dialog)));
            }
        }
        Ok(response)
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("agito", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Drives a real browser. Call navigate, then snapshot to read the page as an \
                 accessibility tree whose refs (e1, e2, ...) click, type and the other \
                 interaction tools take. Refs last until the page changes: take a new snapshot \
                 after navigating.",
            )
    }
}

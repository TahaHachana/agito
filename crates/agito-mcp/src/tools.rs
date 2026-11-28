//! The tools an agent calls. Each acts on the active tab, addresses elements by a ref from the
//! latest snapshot or by a selector, and reports failure as an error result with guidance the
//! agent can act on, never as a protocol error, which would lose the guidance.

use std::sync::Arc;
use std::time::{Duration, Instant};

use agito::{ClickOptions, ElementState, Frame, Locator, MouseButton, Page, ScreenshotOptions};
use base64::Engine;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::ServerOptions;
use crate::session::{Session, SnapshotError, TargetError};

/// How every tool describes the element it acts on.
const TARGET: &str = "A ref from the latest snapshot (e5, or f1e5 for an element inside frame 1), \
    or a selector: CSS by default, or prefixed with xpath=, text=, role=, or data-testid=. A \
    selector needs no snapshot.";

const SNAPSHOT_AFTER: &str = "Append a snapshot of the page after the action.";

/// How long a result waits for a page to say what it is called.
const TITLE_TIMEOUT: Duration = Duration::from_secs(2);

fn text(message: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(message.into())])
}

fn error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// The page's title, or `None` when it will not say within [`TITLE_TIMEOUT`]: a title is a
/// nicety, not worth delaying or failing the call that was only naming the page.
async fn try_title(page: &Page) -> Option<String> {
    match tokio::time::timeout(TITLE_TIMEOUT, page.title()).await {
        Ok(Ok(title)) if !title.is_empty() => Some(title),
        _ => None,
    }
}

/// A page as `url | title`, or its URL alone.
async fn describe(page: &Page) -> String {
    let url = page.url();
    match try_title(page).await {
        Some(title) => format!("{url} | {title}"),
        None => url,
    }
}

/// Refuses a `file:` URL unless the server allows it: a page could otherwise talk an agent
/// into reading the machine's files back through a snapshot.
fn refuse_url(options: &ServerOptions, url: &str) -> Option<String> {
    let scheme = url.split(':').next().unwrap_or_default();
    (!options.allow_unrestricted_file_access && scheme.eq_ignore_ascii_case("file")).then(|| {
        format!(
            "Refused to open '{url}': file: URLs are blocked. Start the server with \
             --allow-unrestricted-file-access to allow them."
        )
    })
}

/// The `agito mcp` server: one browser session, and the tools that drive it.
#[derive(Clone)]
pub struct AgitoServer {
    options: Arc<ServerOptions>,
    session: Arc<Mutex<Session>>,
    /// Held for the whole of each tool call, so calls run one at a time in the order they
    /// arrived. MCP lets a client send calls without waiting, and agents do issue several at
    /// once; run together, a click could land before the snapshot naming its ref.
    calls: Arc<Mutex<()>>,
    tool_router: ToolRouter<Self>,
}

impl AgitoServer {
    /// A server that starts its browser, with `options`, on the first call that needs one.
    pub fn new(options: Arc<ServerOptions>) -> Self {
        AgitoServer {
            session: Arc::new(Mutex::new(Session::new(options.clone()))),
            calls: Arc::new(Mutex::new(())),
            options,
            tool_router: Self::tool_router(),
        }
    }

    pub(crate) fn session(&self) -> Arc<Mutex<Session>> {
        self.session.clone()
    }

    async fn page(&self) -> Result<Page, CallToolResult> {
        self.session
            .lock()
            .await
            .active_page()
            .await
            .map_err(|e| error(format!("Could not open a page: {e}")))
    }

    /// The active page and a locator for `target` on it.
    async fn target(&self, target: &str) -> Result<(Page, Locator), CallToolResult> {
        if target.trim().is_empty() {
            return Err(error(
                "ref is required: pass a ref from the latest snapshot or a selector.",
            ));
        }
        let mut session = self.session.lock().await;
        let page = session
            .active_page()
            .await
            .map_err(|e| error(format!("Could not open a page: {e}")))?;
        let snapshot = session.snapshot_of(&page);
        drop(session);
        let locator = snapshot
            .lock()
            .await
            .resolve(&page, target, None::<&Frame>)
            .map_err(|e| error(e.message()))?;
        Ok((page, locator))
    }

    /// Takes a snapshot of `page` and renders it as a tool's text.
    async fn snapshot_text(
        &self,
        page: &Page,
        root_ref: Option<&str>,
        max_depth: Option<usize>,
    ) -> Result<String, String> {
        let state = self.session.lock().await.snapshot_of(page);
        let mut state = state.lock().await;
        match state.take(page, None, root_ref, max_depth).await {
            Ok((text, None)) => Ok(text),
            Ok((text, Some(frame))) => Ok(format!("Scoped to frame {}\n\n{text}", frame.url())),
            Err(SnapshotError::Target(TargetError::NoSnapshot)) => {
                Err("No snapshot has been taken. Call snapshot without root_ref first.".into())
            }
            Err(SnapshotError::Target(TargetError::Stale(r))) => Err(format!(
                "Ref '{r}' is not in the latest snapshot. Take a full snapshot to refresh refs."
            )),
            Err(SnapshotError::Browser(e)) => Err(format!("Snapshot failed: {e}")),
        }
    }

    /// Finishes an action's result: `message`, then a snapshot when one was asked for.
    async fn finish(&self, page: &Page, message: String, snapshot: Option<bool>) -> CallToolResult {
        if snapshot != Some(true) {
            return text(message);
        }
        match self.snapshot_text(page, None, None).await {
            Ok(tree) => text(format!("{message}\n\nSnapshot:\n{tree}")),
            Err(e) => text(format!("{message}\n\nThe snapshot after it failed: {e}")),
        }
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
        F: FnOnce(Locator) -> Fut,
        Fut: Future<Output = agito::Result<()>>,
    {
        let (page, locator) = match self.target(target).await {
            Ok(found) => found,
            Err(result) => return result,
        };
        match action(locator).await {
            Ok(()) => self.finish(&page, done, snapshot).await,
            Err(e) => error(format!("{target}: {e}")),
        }
    }

    /// Runs a history navigation and reports where it went.
    async fn history(
        &self,
        snapshot: Option<bool>,
        verb: &str,
        none: &str,
        go: impl AsyncFnOnce(&Page) -> agito::Result<Option<agito::Response>>,
    ) -> CallToolResult {
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        match go(&page).await {
            Ok(response) => {
                self.session.lock().await.invalidate(&page);
                let message = match response {
                    None => none.to_owned(),
                    Some(_) => format!("Navigated {verb} to {}", describe(&page).await),
                };
                self.finish(&page, message, snapshot).await
            }
            Err(e) => error(format!("Go {verb} failed: {e}")),
        }
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
    /// Wait until this text appears anywhere on the page.
    text: Option<String>,
    /// Wait until this text is gone from the page.
    text_gone: Option<String>,
}

/// `evaluate`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct EvaluateArgs {
    /// The JavaScript expression to evaluate.
    expression: String,
    /// The element to evaluate against, passed to the function as its argument and as `this`.
    /// Omit to evaluate in the page. A ref from the latest snapshot or a selector.
    r#ref: Option<String>,
}

/// `tab_open`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TabOpenArgs {
    /// URL to open the new tab at. Omit to open a blank tab.
    url: Option<String>,
}

/// `tab_select`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TabSelectArgs {
    /// Zero-based index of the tab to activate.
    index: usize,
}

/// `tab_close`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TabCloseArgs {
    /// Zero-based index of the tab to close. Omit to close the active tab.
    index: Option<usize>,
}

fn parse_button(button: Option<&str>) -> Result<MouseButton, CallToolResult> {
    match button.map(str::to_ascii_lowercase).as_deref() {
        None | Some("left") => Ok(MouseButton::Left),
        Some("right") => Ok(MouseButton::Right),
        Some("middle") => Ok(MouseButton::Middle),
        Some(other) => Err(error(format!(
            "Unknown button '{other}'. Use one of: left, right, middle."
        ))),
    }
}

fn parse_modifiers(modifiers: &[String]) -> Result<u32, CallToolResult> {
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

/// Wraps a value under `result`: structured content must be an object, and one shape for every
/// value keeps a single count as easy to read as a record.
fn evaluation_result(value: serde_json::Value) -> CallToolResult {
    CallToolResult::structured(serde_json::json!({ "result": value }))
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
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        match page.goto(&args.url).await {
            Ok(_) => {
                self.session.lock().await.invalidate(&page);
                let message = format!("Navigated to {}", describe(&page).await);
                self.finish(&page, message, args.snapshot).await
            }
            Err(e) => error(format!("Navigation failed: {e}")),
        }
    }

    #[tool(
        name = "go_back",
        description = "Navigates the active tab back one entry in its history. Reports when there \
            was no entry to go to.",
        annotations(title = "Go back", destructive_hint = false)
    )]
    async fn go_back(&self, Parameters(args): Parameters<SnapshotAfterArgs>) -> CallToolResult {
        self.history(
            args.snapshot,
            "back",
            "No previous history entry; the page did not change.",
            async |page| page.go_back().await,
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
        self.history(
            args.snapshot,
            "forward",
            "No next history entry; the page did not change.",
            async |page| page.go_forward().await,
        )
        .await
    }

    #[tool(
        name = "reload",
        description = "Reloads the active tab and waits for it to finish loading.",
        annotations(title = "Reload", destructive_hint = false)
    )]
    async fn reload(&self, Parameters(args): Parameters<SnapshotAfterArgs>) -> CallToolResult {
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        match page.reload().await {
            Ok(_) => {
                self.session.lock().await.invalidate(&page);
                self.finish(&page, format!("Reloaded {}", page.url()), args.snapshot)
                    .await
            }
            Err(e) => error(format!("Reload failed: {e}")),
        }
    }

    #[tool(
        name = "snapshot",
        description = "Returns a compact indented accessibility tree of the active page. \
            Interactive, named, and focusable elements and iframes carry a ref (e1, e2, ...) that \
            click and type use to address them, and those tools take a selector just as readily; \
            other nodes are printed for context without a ref. Text is printed inline on its \
            element's line, headings show [level=N], links show [url=...], and form controls show \
            [value=\"...\"] and state flags.",
        annotations(
            title = "Accessibility snapshot",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn snapshot(&self, Parameters(args): Parameters<SnapshotArgs>) -> CallToolResult {
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        match self
            .snapshot_text(&page, args.root_ref.as_deref(), args.max_depth)
            .await
        {
            Ok(tree) => text(tree),
            Err(message) => error(message),
        }
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
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        let options = ScreenshotOptions {
            full_page: args.full_page.unwrap_or(false),
            ..Default::default()
        };
        match page.screenshot(options).await {
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
            through the same actionability checks as a plain one.",
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
        self.on_element(&args.r#ref, done, args.snapshot, async |locator| {
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
        self.on_element(&args.r#ref, done, args.snapshot, async |locator| {
            if slowly {
                locator.type_text(&args.text).await?;
            } else {
                locator.fill(&args.text).await?;
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
        self.on_element(&args.r#ref, done, args.snapshot, async |locator| {
            locator.press(&args.key).await
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
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        match page.keyboard().press(&args.key).await {
            Ok(()) => text(format!("Pressed {}", args.key)),
            Err(e) => error(format!("Press key failed: {e}")),
        }
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
        let values: Vec<&str> = args.values.iter().map(String::as_str).collect();
        self.on_element(&args.r#ref, done, args.snapshot, async |locator| {
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
        self.on_element(&args.r#ref, done, args.snapshot, async |locator| {
            locator.set_checked(args.checked).await
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
        let done = format!(
            "{} reached state {}",
            args.r#ref,
            args.state.to_ascii_lowercase()
        );
        self.on_element(&args.r#ref, done, None, async |l| {
            l.wait_for(state, None).await
        })
        .await
    }

    #[tool(
        name = "wait_for",
        description = "Waits for a page condition: a fixed time, text to appear, or text to \
            disappear. Provide exactly one of time, text, or text_gone.",
        annotations(
            title = "Wait for a page condition",
            read_only_hint = true,
            destructive_hint = false
        )
    )]
    async fn wait_for(&self, Parameters(args): Parameters<WaitForArgs>) -> CallToolResult {
        let page = match self.page().await {
            Ok(page) => page,
            Err(result) => return result,
        };
        let (wanted, present) = match (args.time, args.text, args.text_gone) {
            (Some(ms), None, None) => {
                page.wait_for_timeout(Duration::from_millis(ms)).await;
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
        let timeout = page.context().options().action_timeout;
        let deadline = Instant::now() + timeout;
        loop {
            if page.evaluate::<bool>(&probe).await.ok() == Some(present) {
                return text(if present {
                    format!("Text appeared: {wanted}")
                } else {
                    format!("Text gone: {wanted}")
                });
            }
            if Instant::now() >= deadline {
                return error(format!(
                    "Wait failed: timed out after {} ms waiting for '{wanted}' to {}.",
                    timeout.as_millis(),
                    if present { "appear" } else { "disappear" }
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    #[tool(
        name = "evaluate",
        description = "Evaluates a JavaScript expression and returns its result as structured \
            JSON under a \"result\" key, so an expression may return a value of any shape: a \
            number, a string, an array, or an object. With no ref it runs in the page; with a ref \
            or a selector the expression must be a function, and runs against that element, \
            passed as its argument. Results that cannot be serialized (undefined, functions, DOM \
            nodes) come back as null.",
        annotations(title = "Evaluate JavaScript", destructive_hint = true)
    )]
    async fn evaluate(&self, Parameters(args): Parameters<EvaluateArgs>) -> CallToolResult {
        if args.expression.trim().is_empty() {
            return error("expression is required.");
        }
        let outcome = match args.r#ref.as_deref().filter(|r| !r.trim().is_empty()) {
            None => {
                let page = match self.page().await {
                    Ok(page) => page,
                    Err(result) => return result,
                };
                page.evaluate::<Option<serde_json::Value>>(&args.expression)
                    .await
            }
            Some(target) => {
                let (_, locator) = match self.target(target).await {
                    Ok(found) => found,
                    Err(result) => return result,
                };
                locator
                    .evaluate::<Option<serde_json::Value>>(&element_function(&args.expression), &[])
                    .await
            }
        };
        match outcome {
            Ok(value) => evaluation_result(value.unwrap_or_default()),
            Err(e) => error(format!("Evaluate failed: {e}")),
        }
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
        for (i, (context, page)) in tabs.iter().enumerate() {
            let mut line = format!("[{i}] {}", describe(page).await);
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
        let page = match self.session.lock().await.open_tab().await {
            Ok(page) => page,
            Err(e) => return error(format!("Opening tab failed: {e}")),
        };
        if let Some(url) = &url
            && let Err(e) = page.goto(url).await
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
    async fn tab_select(&self, Parameters(args): Parameters<TabSelectArgs>) -> CallToolResult {
        let selected = self.session.lock().await.select_tab(args.index);
        match selected {
            Ok(page) => text(format!("Selected tab {}: {}", args.index, page.url())),
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
        // tokio's mutex is fair: waiting calls take their turn in arrival order.
        let _turn = self.calls.lock().await;
        let call = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        self.tool_router.call(call).await
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

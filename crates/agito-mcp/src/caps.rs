//! The tool groups added on request with `--caps`: coordinate input, isolated contexts and
//! request mocking.
//!
//! Every tool the server lists is described to the client at the start of every conversation,
//! whether it is called or not, so the groups a session either leans on throughout or never
//! touches at all are left out until asked for. Naming a group only ever adds tools.

use std::collections::HashMap;
use std::time::Duration;

use agito::{ClickOptions, Viewport};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::routes::{self, Rule};
use crate::tools::{AgitoServer, error, parse_button, parse_modifiers, refuse_url, text};

/// An optional tool group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// `click_xy`, `hover_xy`, `move_xy`, `scroll_xy`, `drag` and `resize`: input at viewport
    /// coordinates, for canvases and custom surfaces the accessibility tree cannot describe.
    Coordinates,
    /// `context_list`, `context_create`, `context_select` and `context_close`: isolated
    /// contexts, each with its own cookies and storage.
    Contexts,
    /// `route_fulfill`, `route_abort`, `route_continue`, `unroute` and `route_list`: mocking
    /// and blocking requests.
    Routing,
    /// `trace_start`, `trace_stop`, `har_start`, `har_stop`, `video_start` and `video_stop`:
    /// recording what happened to files in the output directory.
    Recording,
}

impl Capability {
    /// Every group, by its `--caps` name.
    pub const ALL: [(&str, Capability); 4] = [
        ("coordinates", Capability::Coordinates),
        ("contexts", Capability::Contexts),
        ("routing", Capability::Routing),
        ("recording", Capability::Recording),
    ];

    /// Parses a comma-separated list, as `--caps` and `AGITO_MCP_CAPS` take it.
    pub fn parse_list(list: &str) -> Result<Vec<Capability>, String> {
        list.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::parse)
            .collect()
    }
}

impl std::str::FromStr for Capability {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let name = s.trim().to_ascii_lowercase();
        if let Some((_, cap)) = Capability::ALL.iter().find(|(n, _)| *n == name) {
            return Ok(*cap);
        }
        Err(format!(
            "unknown tool group '{s}': expected coordinates, contexts, routing or recording"
        ))
    }
}

const X: &str = "The x coordinate in CSS pixels from the left edge of the viewport.";
const Y: &str = "The y coordinate in CSS pixels from the top edge of the viewport.";

/// `click_xy`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct ClickXyArgs {
    #[schemars(description = X)]
    x: f64,
    #[schemars(description = Y)]
    y: f64,
    /// Double-click instead of a single click.
    double: Option<bool>,
    /// The mouse button: left (default), right, or middle.
    button: Option<String>,
    /// Modifier keys held during the click: Alt, Control, Meta, Shift.
    modifiers: Option<Vec<String>>,
}

/// Arguments for the tools that take a point.
#[derive(Deserialize, JsonSchema)]
pub struct PointArgs {
    #[schemars(description = X)]
    x: f64,
    #[schemars(description = Y)]
    y: f64,
}

/// `scroll_xy`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct ScrollXyArgs {
    #[schemars(description = X)]
    x: f64,
    #[schemars(description = Y)]
    y: f64,
    /// Horizontal scroll amount in CSS pixels; positive scrolls right.
    delta_x: f64,
    /// Vertical scroll amount in CSS pixels; positive scrolls down.
    delta_y: f64,
}

/// `resize`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct ResizeArgs {
    /// The new viewport width in CSS pixels.
    width: u32,
    /// The new viewport height in CSS pixels.
    height: u32,
}

/// `drag`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct DragArgs {
    /// The element to drag. A ref from the latest snapshot or a selector.
    start_ref: Option<String>,
    /// The drop target. A ref from the latest snapshot or a selector.
    end_ref: Option<String>,
    /// The x coordinate to drag from, in CSS pixels.
    start_x: Option<f64>,
    /// The y coordinate to drag from, in CSS pixels.
    start_y: Option<f64>,
    /// The x coordinate to drop at, in CSS pixels.
    end_x: Option<f64>,
    /// The y coordinate to drop at, in CSS pixels.
    end_y: Option<f64>,
    /// Number of intermediate pointer moves between press and release. Default 10.
    steps: Option<u32>,
    /// Milliseconds to hold the button down before moving, for libraries that threshold drag
    /// starts.
    hold_ms: Option<u64>,
}

/// Arguments for the tools that take a context name.
#[derive(Deserialize, JsonSchema)]
pub struct NameArgs {
    /// The context's name.
    name: String,
}

const PATTERN: &str = "URL pattern to match: an exact URL, a glob with * covering the whole URL, \
    or a substring.";

/// `route_fulfill`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct FulfillArgs {
    #[schemars(description = PATTERN)]
    url_pattern: String,
    /// HTTP status code to return. Defaults to 200.
    status: Option<u16>,
    /// Response body to return.
    body: Option<String>,
    /// Content-Type of the response, e.g. application/json.
    content_type: Option<String>,
    /// Additional response headers as name/value pairs.
    headers: Option<HashMap<String, String>>,
}

/// `route_abort`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct AbortArgs {
    #[schemars(description = PATTERN)]
    url_pattern: String,
    /// The failure the page sees: aborted, accessdenied, connectionrefused, connectionreset,
    /// timedout, connectionfailed, namenotresolved, internetdisconnected, addressunreachable,
    /// blockedbyclient, or blockedbyresponse. Anything else is a generic failure.
    error_code: Option<String>,
}

/// `route_continue`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct ContinueArgs {
    #[schemars(description = PATTERN)]
    url_pattern: String,
    /// Override the request URL.
    url: Option<String>,
    /// Override the HTTP method, e.g. POST.
    method: Option<String>,
    /// Override or add request headers as name/value pairs.
    headers: Option<HashMap<String, String>>,
    /// Override the request body.
    post_data: Option<String>,
}

/// `unroute`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct UnrouteArgs {
    /// The URL pattern whose rule to remove, spelled as it was registered.
    url_pattern: String,
}

#[tool_router(router = coordinate_tools, vis = "pub(crate)")]
impl AgitoServer {
    #[tool(
        name = "click_xy",
        description = "Clicks at a viewport coordinate with trusted browser input. The browser's \
            native hit test decides the target, so this works on canvas and custom-rendered \
            surfaces that have no refs. Read positions from a screenshot or \
            getBoundingClientRect(); both use this coordinate space.",
        annotations(title = "Click at coordinates", destructive_hint = true)
    )]
    async fn click_xy(&self, Parameters(args): Parameters<ClickXyArgs>) -> CallToolResult {
        let button = match parse_button(args.button.as_deref()) {
            Ok(button) => button,
            Err(result) => return result,
        };
        let modifiers = match parse_modifiers(args.modifiers.as_deref().unwrap_or_default()) {
            Ok(bits) => bits,
            Err(result) => return result,
        };
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let mouse = tab.page.mouse().clone();
        let (x, y) = (args.x, args.y);
        let double = args.double == Some(true);
        self.act(tab, None, async move {
            let options = ClickOptions {
                button,
                modifiers,
                ..Default::default()
            };
            let clicked = if double {
                mouse.dblclick(x, y, options).await
            } else {
                mouse.click(x, y, options).await
            };
            clicked
                .map(|()| {
                    let verb = if double { "Double-clicked" } else { "Clicked" };
                    format!("{verb} at ({x}, {y})")
                })
                .map_err(|e| format!("Click at ({x}, {y}) failed: {e}"))
        })
        .await
    }

    #[tool(
        name = "hover_xy",
        description = "Moves the pointer to a viewport coordinate, triggering hover effects at \
            that point. Works on surfaces without refs, such as canvas rendering.",
        annotations(title = "Hover at coordinates", destructive_hint = true)
    )]
    async fn hover_xy(&self, Parameters(args): Parameters<PointArgs>) -> CallToolResult {
        self.move_pointer(
            args.x,
            args.y,
            format!("Hovering at ({}, {})", args.x, args.y),
        )
        .await
    }

    #[tool(
        name = "move_xy",
        description = "Moves the pointer to a viewport coordinate without pressing a button. \
            Useful for positioning before a wheel scroll or observing pointer-tracking UI.",
        annotations(title = "Move pointer to coordinates", destructive_hint = true)
    )]
    async fn move_xy(&self, Parameters(args): Parameters<PointArgs>) -> CallToolResult {
        self.move_pointer(
            args.x,
            args.y,
            format!("Moved pointer to ({}, {})", args.x, args.y),
        )
        .await
    }

    #[tool(
        name = "scroll_xy",
        description = "Dispatches a trusted mouse wheel event at a viewport coordinate, scrolling \
            whatever scrollable region is under that point. Positive delta_y scrolls down, \
            positive delta_x scrolls right. Use this to bring canvas-painted content into view \
            when scroll_into_view has no ref to target.",
        annotations(title = "Wheel scroll at coordinates", destructive_hint = true)
    )]
    async fn scroll_xy(&self, Parameters(args): Parameters<ScrollXyArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let mouse = tab.page.mouse().clone();
        self.act(tab, None, async move {
            let ScrollXyArgs {
                x,
                y,
                delta_x,
                delta_y,
            } = args;
            async {
                mouse.move_to(x, y, 1).await?;
                mouse.wheel(delta_x, delta_y).await
            }
            .await
            .map(|()| format!("Scrolled ({delta_x}, {delta_y}) at ({x}, {y})"))
            .map_err(|e| format!("Scroll at ({x}, {y}) failed: {e}"))
        })
        .await
    }

    #[tool(
        name = "drag",
        description = "Drags from one point to another with trusted input: press, intermediate \
            moves, release. Address the endpoints either by elements (start_ref/end_ref, each a \
            ref or a selector, each scrolled into view before the pointer goes to it) or by \
            viewport coordinates (start_x/start_y/end_x/end_y); coordinates work on canvas \
            drop-zones that have no refs. Intermediate moves are always emitted because drag \
            libraries commonly require observed movement.",
        annotations(title = "Drag and drop", destructive_hint = true)
    )]
    async fn drag(&self, Parameters(args): Parameters<DragArgs>) -> CallToolResult {
        let by_ref = args.start_ref.is_some() || args.end_ref.is_some();
        let by_point = args.start_x.is_some()
            || args.start_y.is_some()
            || args.end_x.is_some()
            || args.end_y.is_some();
        if by_ref == by_point {
            return error(
                "Provide either start_ref and end_ref, or all of start_x, start_y, end_x, end_y, \
                 but not both.",
            );
        }
        let steps = args.steps.unwrap_or(10).max(1);
        let hold = args.hold_ms.filter(|ms| *ms > 0).map(Duration::from_millis);
        // Each endpoint: a fixed point, or an element measured when the pointer goes to it.
        let (start, end, from, to) = if by_ref {
            let (Some(start_ref), Some(end_ref)) = (args.start_ref, args.end_ref) else {
                return error("Both start_ref and end_ref are required when dragging by ref.");
            };
            let start = match self.target(&start_ref).await {
                Ok((_, locator)) => Endpoint::Element(locator, start_ref.clone()),
                Err(result) => return result,
            };
            let end = match self.target(&end_ref).await {
                Ok((_, locator)) => Endpoint::Element(locator, end_ref.clone()),
                Err(result) => return result,
            };
            (start, end, start_ref, end_ref)
        } else {
            let (Some(sx), Some(sy), Some(ex), Some(ey)) =
                (args.start_x, args.start_y, args.end_x, args.end_y)
            else {
                return error(
                    "All of start_x, start_y, end_x, end_y are required when dragging by \
                     coordinates.",
                );
            };
            (
                Endpoint::Point(sx, sy),
                Endpoint::Point(ex, ey),
                format!("({sx}, {sy})"),
                format!("({ex}, {ey})"),
            )
        };
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let mouse = tab.page.mouse().clone();
        self.act(tab, None, async move {
            let (sx, sy) = start.position().await?;
            let pressed = async {
                mouse.move_to(sx, sy, 1).await?;
                mouse.down(ClickOptions::default()).await
            };
            pressed.await.map_err(|e| format!("Drag failed: {e}"))?;
            let moved = async {
                if let Some(hold) = hold {
                    tokio::time::sleep(hold).await;
                }
                let (ex, ey) = end.position().await?;
                mouse
                    .move_to(ex, ey, steps)
                    .await
                    .map_err(|e| format!("Drag failed: {e}"))
            }
            .await;
            // The button is released whatever happened, so the page is not left mid-drag.
            let released = mouse.up(ClickOptions::default()).await;
            moved?;
            released.map_err(|e| format!("Drag failed: {e}"))?;
            Ok(format!("Dragged {from} to {to}"))
        })
        .await
    }

    #[tool(
        name = "resize",
        description = "Resizes the page viewport. Use this when controls sit at or beyond the \
            viewport edge; subsequent screenshots and coordinates use the new size.",
        annotations(
            title = "Resize viewport",
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn resize(&self, Parameters(args): Parameters<ResizeArgs>) -> CallToolResult {
        if args.width == 0 || args.height == 0 {
            return error("Width and height must be positive.");
        }
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let viewport = Viewport {
            width: args.width,
            height: args.height,
        };
        match tab.page.set_viewport(viewport).await {
            Ok(()) => text(format!(
                "Viewport resized to {}x{}",
                args.width, args.height
            )),
            Err(e) => error(format!(
                "Resize to {}x{} failed: {e}",
                args.width, args.height
            )),
        }
    }
}

/// Where one end of a drag is.
enum Endpoint {
    Point(f64, f64),
    /// An element, by its locator and the target the agent named it by.
    Element(agito::Locator, String),
}

impl Endpoint {
    /// The point to put the pointer at: an element's centre, once scrolled into view.
    async fn position(&self) -> Result<(f64, f64), String> {
        match self {
            Endpoint::Point(x, y) => Ok((*x, *y)),
            Endpoint::Element(locator, name) => {
                locator
                    .scroll_into_view()
                    .await
                    .map_err(|e| format!("{name}: {e}"))?;
                let bounds = locator
                    .bounding_box()
                    .await
                    .map_err(|e| format!("{name}: {e}"))?
                    .ok_or_else(|| format!("Element {name} has no visible bounding box."))?;
                Ok((
                    bounds.x + bounds.width / 2.0,
                    bounds.y + bounds.height / 2.0,
                ))
            }
        }
    }
}

impl AgitoServer {
    async fn move_pointer(&self, x: f64, y: f64, done: String) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let mouse = tab.page.mouse().clone();
        self.act(tab, None, async move {
            mouse
                .move_to(x, y, 1)
                .await
                .map(|()| done)
                .map_err(|e| format!("Pointer move to ({x}, {y}) failed: {e}"))
        })
        .await
    }
}

/// `browser_attach`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct AttachArgs {
    /// The browser's debugging endpoint, e.g. http://127.0.0.1:9222, or its CDP WebSocket URL.
    endpoint: String,
}

#[tool_router(router = attach_tools, vis = "pub(crate)")]
impl AgitoServer {
    #[tool(
        name = "browser_attach",
        description = "Connects to a browser that is already running and drives what is open in \
            it, instead of the browser this server started, which is closed. The attached browser \
            is never closed by the server. Refs, mock rules, logs and named contexts do not carry \
            over: take a snapshot afterwards.",
        annotations(title = "Attach to a running browser", destructive_hint = true)
    )]
    async fn browser_attach(&self, Parameters(args): Parameters<AttachArgs>) -> CallToolResult {
        if args.endpoint.trim().is_empty() {
            return error("endpoint is required.");
        }
        match self.session.lock().await.attach(&args.endpoint).await {
            Ok(tabs) => text(format!(
                "Attached to the browser at {}; {tabs} tab{} open. Take a snapshot before \
                 addressing elements.",
                args.endpoint,
                if tabs == 1 { " is" } else { "s are" }
            )),
            Err(e) => error(format!("Attaching to {} failed: {e}", args.endpoint)),
        }
    }
}

#[tool_router(router = context_tools, vis = "pub(crate)")]
impl AgitoServer {
    #[tool(
        name = "context_list",
        description = "Lists the open browser contexts. The active context is marked with an \
            asterisk.",
        annotations(
            title = "List contexts",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn context_list(&self) -> CallToolResult {
        let session = self.session.lock().await;
        let active = session.active_context_name().to_owned();
        let names = session.context_names();
        if names.is_empty() {
            return text(format!(
                "No contexts are open yet; '{active}' becomes active on first use."
            ));
        }
        let lines: Vec<String> = names
            .iter()
            .map(|n| format!("{}{n}", if *n == active { "* " } else { "  " }))
            .collect();
        text(lines.join("\n"))
    }

    #[tool(
        name = "context_create",
        description = "Creates a new isolated context with its own cookies and storage and makes \
            it active. Fails if a context with that name already exists. Take a snapshot after \
            switching before addressing elements.",
        annotations(title = "Create a context", destructive_hint = true)
    )]
    async fn context_create(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        if args.name.trim().is_empty() {
            return error("name is required.");
        }
        match self.session.lock().await.create_context(&args.name).await {
            Ok(()) => text(format!("Created context '{}'.", args.name)),
            Err(message) => error(message),
        }
    }

    #[tool(
        name = "context_select",
        description = "Makes an existing context active, so the page tools that follow act on \
            one of its tabs. tab_list shows every context's tabs either way, and selecting one of \
            them switches context on its own.",
        annotations(title = "Select a context", destructive_hint = false)
    )]
    async fn context_select(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        match self.session.lock().await.select_context(&args.name) {
            Ok(()) => text(format!("Switched to context '{}'.", args.name)),
            Err(message) => error(message),
        }
    }

    #[tool(
        name = "context_close",
        description = "Closes the named context and all its tabs, which drops them from tab_list, \
            along with its mock rules. If the active context is closed, the default context \
            becomes active.",
        annotations(title = "Close a context", destructive_hint = true)
    )]
    async fn context_close(&self, Parameters(args): Parameters<NameArgs>) -> CallToolResult {
        match self.session.lock().await.close_context(&args.name).await {
            Ok(true) => text(format!("Closed context '{}'.", args.name)),
            Ok(false) => error(format!(
                "No open context is named '{}'. Call context_list to see the open ones.",
                args.name
            )),
            Err(message) => error(format!("Closing context '{}' failed: {message}", args.name)),
        }
    }
}

#[tool_router(router = routing_tools, vis = "pub(crate)")]
impl AgitoServer {
    #[tool(
        name = "route_fulfill",
        description = "Intercepts requests matching the URL pattern on the active context and \
            answers them with a mock response instead of hitting the network. Re-registering the \
            same pattern replaces its rule; when several patterns match, the most recently \
            registered one handles the request.",
        annotations(title = "Mock a response", destructive_hint = true)
    )]
    async fn route_fulfill(&self, Parameters(args): Parameters<FulfillArgs>) -> CallToolResult {
        let headers = args.headers.unwrap_or_default();
        // A redirect to a file would read the machine's files back into the page.
        let location = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("location"))
            .map(|(_, v)| v.as_str());
        if let Some(refusal) = location.and_then(|l| refuse_url(&self.options, l)) {
            return error(refusal);
        }
        let status = args.status.unwrap_or(200);
        let rule = Rule::Fulfill {
            status,
            body: args.body.unwrap_or_default(),
            content_type: args.content_type,
            headers,
        };
        self.register(&args.url_pattern, rule, || {
            format!("Mocking '{}' with status {status}.", args.url_pattern)
        })
        .await
    }

    #[tool(
        name = "route_abort",
        description = "Intercepts requests matching the URL pattern on the active context and \
            aborts them, so the page sees a failed request. Re-registering the same pattern \
            replaces its rule.",
        annotations(title = "Block requests", destructive_hint = true)
    )]
    async fn route_abort(&self, Parameters(args): Parameters<AbortArgs>) -> CallToolResult {
        let rule = Rule::Abort {
            error_code: args.error_code,
        };
        self.register(&args.url_pattern, rule, || {
            format!("Blocking '{}'.", args.url_pattern)
        })
        .await
    }

    #[tool(
        name = "route_continue",
        description = "Intercepts requests matching the URL pattern on the active context and \
            lets them continue with the given overrides applied (URL, method, headers, or body). \
            Re-registering the same pattern replaces its rule.",
        annotations(title = "Override requests", destructive_hint = true)
    )]
    async fn route_continue(&self, Parameters(args): Parameters<ContinueArgs>) -> CallToolResult {
        if let Some(refusal) = args
            .url
            .as_deref()
            .and_then(|u| refuse_url(&self.options, u))
        {
            return error(refusal);
        }
        let rule = Rule::Continue {
            url: args.url,
            method: args.method,
            headers: args.headers.unwrap_or_default(),
            post_data: args.post_data,
        };
        self.register(&args.url_pattern, rule, || {
            format!("Continuing '{}' with overrides.", args.url_pattern)
        })
        .await
    }

    #[tool(
        name = "unroute",
        description = "Removes the mock rule for the URL pattern on the active context, so \
            matching requests hit the network again. Reports when no rule was registered for the \
            pattern.",
        annotations(title = "Remove a mock", destructive_hint = false)
    )]
    async fn unroute(&self, Parameters(args): Parameters<UnrouteArgs>) -> CallToolResult {
        let routes = match self.session.lock().await.active_routes().await {
            Ok((_, routes)) => routes,
            Err(e) => return error(format!("Could not open a context: {e}")),
        };
        let removed = routes.lock().unwrap().remove(&args.url_pattern);
        text(if removed {
            format!("Removed the mock for '{}'.", args.url_pattern)
        } else {
            format!("No mock was registered for '{}'.", args.url_pattern)
        })
    }

    #[tool(
        name = "route_list",
        description = "Lists the mock rules registered on the active context, each with its \
            pattern and action, in the order they were registered.",
        annotations(
            title = "List mocks",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn route_list(&self) -> CallToolResult {
        let routes = match self.session.lock().await.active_routes().await {
            Ok((_, routes)) => routes,
            Err(e) => return error(format!("Could not open a context: {e}")),
        };
        let rules = routes.lock().unwrap().list();
        if rules.is_empty() {
            return text("No mocks are registered on the active context.");
        }
        let lines: Vec<String> = rules.iter().map(|(p, k)| format!("{p} -> {k}")).collect();
        text(lines.join("\n"))
    }
}

impl AgitoServer {
    async fn register(
        &self,
        pattern: &str,
        rule: Rule,
        done: impl FnOnce() -> String,
    ) -> CallToolResult {
        if pattern.is_empty() {
            return error("url_pattern is required.");
        }
        let (context, routes) = match self.session.lock().await.active_routes().await {
            Ok(found) => found,
            Err(e) => return error(format!("Could not open a context: {e}")),
        };
        match routes::register(&context, &routes, pattern, rule).await {
            Ok(()) => text(done()),
            Err(e) => error(format!("Registering '{pattern}' failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_parse_from_a_list() {
        assert_eq!(
            Capability::parse_list("coordinates, Routing,,contexts"),
            Ok(vec![
                Capability::Coordinates,
                Capability::Routing,
                Capability::Contexts
            ])
        );
        assert_eq!(
            Capability::parse_list("recording"),
            Ok(vec![Capability::Recording])
        );
        assert!(Capability::parse_list("teleport").is_err());
        assert_eq!(Capability::parse_list(""), Ok(vec![]));
    }
}

/// `trace_start`'s arguments.
#[derive(Deserialize, JsonSchema)]
pub struct TraceStartArgs {
    /// Take a screenshot after each action and navigation. Defaults to true.
    screenshots: Option<bool>,
    /// Also record the browser's performance timeline, for Chrome DevTools or Perfetto.
    /// Defaults to false: it covers the whole browser, and one runs at a time.
    timeline: Option<bool>,
}

/// Arguments for the tools that write a file.
#[derive(Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Where to write the file, relative to the server's output directory. Omit for a fresh
    /// name there.
    path: Option<String>,
}

#[tool_router(router = recording_tools, vis = "pub(crate)")]
impl AgitoServer {
    #[tool(
        name = "trace_start",
        description = "Begins recording a trace of the active context: each action with its \
            target, duration and error, navigations, console messages, uncaught errors, dialogs, \
            the network log, and a screenshot after each action. Call trace_stop to write it.",
        annotations(title = "Start trace recording", destructive_hint = false)
    )]
    async fn trace_start(&self, Parameters(args): Parameters<TraceStartArgs>) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        let options = agito::TracingOptions {
            title: None,
            screenshots: args.screenshots.unwrap_or(true),
            timeline: args.timeline.unwrap_or(false),
        };
        match tab.page.context().start_tracing(options).await {
            Ok(()) => text("Tracing the active context. Call trace_stop to write the trace."),
            Err(e) => error(format!("Starting the trace failed: {e}")),
        }
    }

    #[tool(
        name = "trace_stop",
        description = "Stops the trace started by trace_start and writes it to a zip file \
            (trace.json, har.json, screenshots). Returns the file's absolute path.",
        annotations(title = "Stop trace recording", destructive_hint = false)
    )]
    async fn trace_stop(&self, Parameters(args): Parameters<WriteArgs>) -> CallToolResult {
        let path = match self.output_path(args.path.as_deref(), "trace", "zip") {
            Ok(path) => path,
            Err(refusal) => return error(refusal),
        };
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        match tab.page.context().stop_tracing(&path).await {
            Ok(()) => text(format!("Trace written to {}", path.display())),
            Err(e) => error(format!("Stopping the trace failed: {e}")),
        }
    }

    #[tool(
        name = "har_start",
        description = "Begins recording the active page's network traffic. Call har_stop to \
            write it as an HTTP Archive (HAR) file.",
        annotations(title = "Start HAR recording", destructive_hint = false)
    )]
    async fn har_start(&self) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        tab.page.start_har();
        text("Recording the active page's network traffic. Call har_stop to write it.")
    }

    #[tool(
        name = "har_stop",
        description = "Stops the recording started by har_start and writes the captured traffic \
            to an HTTP Archive (HAR) file. Returns the file's absolute path.",
        annotations(title = "Stop HAR recording", destructive_hint = false)
    )]
    async fn har_stop(&self, Parameters(args): Parameters<WriteArgs>) -> CallToolResult {
        let path = match self.output_path(args.path.as_deref(), "network", "har") {
            Ok(path) => path,
            Err(refusal) => return error(refusal),
        };
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        match tab.page.stop_har_to(&path) {
            Ok(()) => text(format!("HAR written to {}", path.display())),
            Err(e) => error(format!("Stopping the HAR recording failed: {e}")),
        }
    }

    #[tool(
        name = "video_start",
        description = "Begins recording the active page to a video file (MJPEG in AVI) at the \
            viewport's size, playing in real time. Call video_stop to finish the file. No pointer \
            is drawn.",
        annotations(title = "Start video recording", destructive_hint = false)
    )]
    async fn video_start(&self, Parameters(args): Parameters<WriteArgs>) -> CallToolResult {
        if self.options.record_video.is_some() {
            return error(
                "Every page is already being recorded: the server was started with \
                 --record-video, and each page's video is finished when it closes.",
            );
        }
        let path = match self.output_path(args.path.as_deref(), "video", "avi") {
            Ok(path) => path,
            Err(refusal) => return error(refusal),
        };
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        match tab.page.start_video(&path).await {
            Ok(()) => text(format!(
                "Recording the active page to {}. Call video_stop to finish it.",
                path.display()
            )),
            Err(e) => error(format!("Starting the video failed: {e}")),
        }
    }

    #[tool(
        name = "video_stop",
        description = "Stops the recording started by video_start and finishes the video file. \
            Returns the file's absolute path.",
        annotations(title = "Stop video recording", destructive_hint = false)
    )]
    async fn video_stop(&self) -> CallToolResult {
        let tab = match self.tab().await {
            Ok(tab) => tab,
            Err(result) => return result,
        };
        match tab.page.stop_video().await {
            Ok(path) => text(format!("Video written to {}", path.display())),
            Err(e) => error(format!("Stopping the video failed: {e}")),
        }
    }
}

impl AgitoServer {
    /// Where to write a file the agent named, or a fresh one, inside the output directory.
    fn output_path(
        &self,
        requested: Option<&str>,
        prefix: &str,
        extension: &str,
    ) -> Result<std::path::PathBuf, String> {
        crate::policy::resolve_write(
            &self.options.output_directory(),
            requested,
            prefix,
            extension,
            self.options.allow_unrestricted_file_access,
        )
    }
}

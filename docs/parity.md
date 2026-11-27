# Motus → agito parity

Tracks the port phase by phase. The MVP covers phases 0-4 for Chromium: enough to write and
run real browser tests.

| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, CI, CDP codegen (`xtask`), protocol crate | Done |
| 1 | Transport: sockets (WebSocket, pipe), CDP connection, sessions, capabilities, fake socket | Done (protocol log pending) |
| 2 | Launcher, browser finder, Browser/Context/Page/Frame, handles, input, config | Done for Chromium (see gaps) |
| 3 | Plugins, selectors, Locator + actionability, network/routing/HAR, dialogs/downloads/file chooser | Done except downloads/file chooser |
| 4 | `expect` assertions, `#[agito::test]` harness, samples | MVP done (see gaps) |
| 5 | Firefox / WebDriver BiDi | Not started |
| 6 | Accessibility, performance, coverage, tracing, video | Not started |
| 7 | CLI (`agito`) | Not started |
| 8 | Recorder, codegen, check-selectors | Not started |
| 9 | MCP server | Not started |
| 10 | Visual runner | Not started |

## Ported

| Motus | agito |
|---|---|
| `MotusLauncher.LaunchAsync` / `ConnectAsync` | `agito::launch` / `agito::connect` |
| `LaunchOptions`, `ContextOptions` | same names; `Default` applies config and env |
| `BrowserFinder`, `InstalledBrowsers`, `ChromiumArgs`, `CdpEndpointPoller` | `launch::{finder, args, endpoint}` |
| `BrowserOutputDrain` | `launch::OutputTail` (stderr tail in launch errors) |
| `IBrowser` / `Browser` | `Browser` (Drop kills a launched browser and removes its temp profile) |
| `IBrowserContext` | `BrowserContext`: pages, init scripts, cookies, close |
| `IPage` | `Page`: goto/reload/back/forward, wait_for_load_state/url, evaluate, content, set_content, screenshot, pdf, viewport, console/page-error/dialog events, `get_by_*` |
| `ILocator` / `ActionabilityChecker` | `Locator`: click, dblclick, hover, focus, fill, clear, type_text, press, check/uncheck, select_option, wait_for, queries, first/last/nth, filter_has_text, chained `locator` incl. `..` |
| `IMouse`, `IKeyboard`, `KeyDefinitions` | `Mouse`, `Keyboard` (same US layout table) |
| `IElementHandle` | `ElementHandle` |
| `IFrame`, `IPage.MainFrame` / `Frames`, out-of-process iframe adoption, frame origins | `Frame` (`page.main_frame()`, `page.frames()`, `page.frame(name)`, `frame_by_url`), plus `FrameLocator` |
| Selector strategies: css, xpath, text, data-testid, role, `_text`/`_label`/`_attr` | `selectors::*`, same prefixes and priorities |
| `IPlugin`, `IPluginContext`, `ISelectorStrategy`, `ILifecycleHook` | `plugin::{Plugin, PluginContext, LifecycleHook}`, `selectors::SelectorStrategy` |
| `[MotusPlugin]` + source-generated discovery | `#[agito::plugin]` + `inventory` |
| `Expect.That(...)` locator/page/response assertions, `AssertionRetryHelper` | `agito::expect(...)`, same retry rules and messages |
| `MotusTestBase` + `[MotusTestClass]` retry-on-disconnect, `BrowserFixture` | `#[agito::test]` + `agito::harness` |
| `motus.config.json`, `MOTUS_*` | `agito.config.json`, `AGITO_*` (launch, context, locator, assertions, failure subsets) |
| `FailureCapture` (screenshot on failure) | `harness` (when `failure.screenshot` is set) |
| Motus.Samples (todo page object, forms) | `samples/agito-samples` |
| `NetworkManager`, `IRequest`, `IResponse` (+ body), request/response events | `network::{Request, Response}`, `page.on_request` / `on_response` / `on_request_finished` / `on_request_failed` |
| `WaitForRequestAsync` / `WaitForResponseAsync`, network idle | `page.wait_for_request` / `wait_for_response` (eager), `page.wait_for_network_idle` |
| `IRoute`, `RouteAsync` / `UnrouteAsync` on page and context | `network::Route` (`fulfill`, `continue_`, `abort`), `page.route` / `context.route`, `unroute(RouteId)`, `unroute_all` |
| `HarRecorder`, `StartHarRecordingAsync` / `StopHarRecordingAsync` | `page.start_har` / `stop_har` / `stop_har_to` |

## Known gaps in the MVP

- **Not yet ported:**
  - From the page: downloads, file chooser, popups and target adoption, touch input, natural
    mouse motion, the cursor overlay.
  - Exposed bindings, storage state, permissions, geolocation, HTTP credentials.
  - Browser pool, heartbeat, and signal handlers.
- **Platforms:** Windows launches over a WebSocket port. The shared test browser is killed at
  process exit only on Unix, where it is tied to the pipe.
- **Assertions:** `has`/`has_not` locator filters and the accessibility-name/role
  assertions are missing.

## Deliberate behavior differences

- An event key subscribed twice fans out to both subscribers.
- Unknown CDP enum values decode to `UnrecognizedValue`.
- Locator refinements apply in the order chained. In Motus, `HasText` and `Nth` applied to
  the final element set wherever they were written.
- `text=` matches the innermost elements containing the text, not their ancestors too.
- The pointer hit-test runs in the element's own tree, so elements inside shadow roots can be
  clicked.
- `select_option` also matches by option label, and reads the selection back after setting
  every option.
- History navigations (`go_back`, `go_forward`, `reload`) wait on the committed document's
  loader, so pages restored from the back-forward cache finish immediately.
- Chrome's "Session with given id not found" and "Target closed" errors count as a lost
  browser, so the harness retries them.
- Profiles left by test binaries are swept on the next launch.

- `page.frame_locator(selector)` / `locator.content_frame()` (Playwright-style) find the iframe
  afresh on every use. Motus only offered frame objects, which go stale when an iframe is
  replaced.
- Selector strategies resolve against a `Frame` instead of a page
  (`SelectorStrategy::resolve(&self, selector, frame: &Frame, pierce)`).
- Elements in a child frame are clicked at page coordinates computed from each `<iframe>`'s
  content box up the chain, for same-process and out-of-process frames alike. Motus only
  offset out-of-process frames, relying on box models for the rest.
- `frame.wait_for_load_state` falls back to `document.readyState` for a frame whose lifecycle
  events were missed (an out-of-process iframe adopted after it loaded).

- `wait_for_request` / `wait_for_response` subscribe when called rather than when awaited,
  so the waiter can be created before the action that triggers the request.
- Route patterns use agito's globs (`*` within a path segment, `**` across). Like Motus, a
  pattern without `*` matches any URL containing it.
- `unroute` takes the `RouteId` that `route` returned, not the pattern string.
- Each page opens in its own window, so a page never becomes a hidden background tab.

## Inherited as-is

- `receives_events` accepts a hit on any descendant of the target's parent, so an element
  directly under `<body>` is never reported as covered.

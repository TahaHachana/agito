# Motus → agito parity

Tracks the port phase by phase. The decisions and the order of work are in
[roadmap.md](roadmap.md). The MVP covers phases 0-4 for Chromium: enough to write and
run real browser tests.

| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, CI, CDP codegen (`xtask`), protocol crate | Done |
| 1 | Transport: sockets (WebSocket, pipe), CDP connection, sessions, capabilities, fake socket, protocol log | Done |
| 2 | Launcher, browser finder, Browser/Context/Page/Frame, handles, input, config | Done for Chromium (see gaps) |
| 3 | Plugins, selectors, Locator + actionability, network/routing/HAR, dialogs/downloads/file chooser | Done |
| 4 | `expect` assertions, `#[agito::test]` harness, samples | MVP done (see gaps) |
| 5 | Firefox / WebDriver BiDi | Deferred until there is demand ([why](roadmap.md#phase-5-firefox--webdriver-bidi-deferred)) |
| 6 | Accessibility, performance, coverage, tracing, video | In progress: accessibility done |
| 7 | CLI (`agito`) | Done; `run` settings in `agito.config.json` left |
| 8 | Recorder, codegen, check-selectors | Not started |
| 9 | MCP server | In progress: stdio, the always-available tools and the `--caps` groups; HTTP, the security policy and attaching next |
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
| `ILocator` / `ActionabilityChecker` | `Locator`: click, dblclick, hover, focus, fill, clear, type_text, press, check/uncheck, select_option, wait_for, queries, first/last/nth, filter_has_text, filter_has_not_text, filter_has, filter_has_not, chained `locator` incl. `..` |
| `IMouse`, `IKeyboard`, `KeyDefinitions` | `Mouse`, `Keyboard` (same US layout table) |
| `IElementHandle` | `ElementHandle` |
| `IFrame`, `IPage.MainFrame` / `Frames`, out-of-process iframe adoption, frame origins | `Frame` (`page.main_frame()`, `page.frames()`, `page.frame(name)`, `frame_by_url`), plus `FrameLocator` |
| Selector strategies: css, xpath, text, data-testid, role, `_text`/`_label`/`_attr` | `selectors::*`, same prefixes and priorities |
| `IPlugin`, `IPluginContext`, `ISelectorStrategy`, `ILifecycleHook` | `plugin::{Plugin, PluginContext, LifecycleHook}`, `selectors::SelectorStrategy` |
| `[MotusPlugin]` + source-generated discovery | `#[agito::plugin]` + `inventory` |
| `Expect.That(...)` locator/page/response assertions (including `ToHaveRole`, `ToHaveAccessibleName`), `AssertionRetryHelper` | `agito::expect(...)`, same retry rules and messages |
| `MotusTestBase` + `[MotusTestClass]` retry-on-disconnect, `BrowserFixture` | `#[agito::test]` + `agito::harness` |
| `motus.config.json`, `MOTUS_*` | `agito.config.json`, `AGITO_*` (launch, context, locator, assertions, failure subsets) |
| `FailureCapture` (screenshot on failure) | `harness` (when `failure.screenshot` is set) |
| Motus.Samples (todo page object, forms) | `samples/agito-samples` |
| `NetworkManager`, `IRequest`, `IResponse` (+ body), request/response events | `network::{Request, Response}`, `page.on_request` / `on_response` / `on_request_finished` / `on_request_failed` |
| `WaitForRequestAsync` / `WaitForResponseAsync`, network idle | `page.wait_for_request` / `wait_for_response` (eager), `page.wait_for_network_idle` |
| `IRoute`, `RouteAsync` / `UnrouteAsync` on page and context | `network::Route` (`fulfill`, `continue_`, `abort`), `page.route` / `context.route`, `unroute(RouteId)`, `unroute_all` |
| `HarRecorder`, `StartHarRecordingAsync` / `StopHarRecordingAsync` | `page.start_har` / `stop_har` / `stop_har_to` |
| `IPage.Popup`, `IBrowserContext.Page`, target discovery and adoption | `page.on_popup` / `wait_for_popup` / `opener`, `context.on_page` / `wait_for_page` |
| `ILocator.SetInputFilesAsync`, `IFileChooser` | `locator.set_input_files` (paths or `FilePayload`s), `page.wait_for_file_chooser` / `on_file_chooser`, `FileChooser::set_files` |
| `motus install` (`BrowserInstaller`) | `agito install` (crate `agito-cli`), writing the markers `launch::finder` reads |
| `motus run` (`TestRunner`, `TestDiscovery`, `ShardSelector`, console, JUnit and HTML reporters, `FlakeHistoryReporter`), `[Quarantine]` | `agito run` (`crates/agito-cli/src/run`), `#[agito::test(quarantine = "reason")]`, `harness::TestRecord` |
| `motus shard merge` (`ShardMerger`) | `agito shard merge` |
| `AccessibilityAuditHook`, `AccessibilityOptions`, `motus run --a11y` | `accessibility::hook`, `AccessibilityOptions` (`ContextOptions::accessibility`), `agito run --a11y` |
| `Motus.Mcp`: `motus mcp`, `SnapshotSerializer`, `PageSnapshotService`, `FrameCollector`, `BrowserSessionManager`, `ActionRunner`, `ActionReport`, `DialogService`, `ConsoleService`, `NetworkService`, `ToolCapabilities`, the always-available tools and the coordinate, context and routing groups | `agito-mcp`: `agito mcp`, `snapshot`, `session`, `runner`, `tab`, `routes`, `caps` and `tools` |
| `Frame.LocatorByBackendNodeId`, `BackendNodeIdSelectorStrategy` | `Frame::locator_by_backend_node_id`, `backend-node-id=` |
| `AccessibilityTreeQuery`, `AccessibilitySnapshotAsync`, the collectors, `ContrastCalculator`, `AccessibilityRuleEngine`, the nine rules, `RunAccessibilityAuditAsync`, `ToPassAccessibilityAuditAsync`, `IAccessibilityRule` | `agito::accessibility`: `page.accessibility_snapshot`, `page.audit_accessibility`, `to_pass_accessibility_audit`, `AccessibilityRule` |
| `IBrowserContext.StorageStateAsync`, `ContextOptions.StorageState` | `BrowserContext::storage_state`, `set_storage_state`, `ContextOptions::storage_state`, `agito::storage` |
| `ProtocolLog`, `ProtocolLogOptions`, `ProtocolLogFormatter`, `MOTUS_PROTOCOL_LOG` | `transport::log`, `ProtocolLogOptions`, `AGITO_PROTOCOL_LOG` |
| `motus update-protocol` (`ProtocolUpdater`, `ProtocolDiffer`) | `cargo xtask update-protocol` |
| `motus screenshot`, `motus pdf`, `BannerRemoval` | `agito screenshot`, `agito pdf`, `js/banners.js` |
| `IDownload`, `IPage.Download` | `Download` (`path`, `save_as`, `failure`, `cancel`, `delete`, `progress`), `page.wait_for_download` / `on_download` |

## Known gaps in the MVP

- **Not yet ported:**
  - From the page: touch input, natural mouse motion, the cursor overlay. Adopting tabs that were already open when connecting is not ported either.
  - Exposed bindings, permissions, geolocation, HTTP credentials.
  - Browser pool, heartbeat, and signal handlers.
  - Harness options `launch = fn`, `perf_budget(...)` and failure tracing.
- **Platforms:** Windows launches over a WebSocket port. The shared test browser is killed at
  process exit only on Unix, where it is tied to the pipe.

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
- Commands pending on a session fail as soon as it detaches (`Target.detachedFromTarget`),
  as in Puppeteer, rather than when the command times out.
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

- New pages attach through browser-level auto-attach and start paused, so a popup's first
  request and scripts already see the context's routes, init scripts and emulation. Motus
  discovered targets (`Target.setDiscoverTargets`) and attached after they started.

- `set_input_files` requires the input only to be attached and enabled, not visible (styled
  upload buttons hide the input); Motus waited for visibility. It also takes paths on disk,
  not only in-memory files, and clearing works (Chrome ignores an empty file list).
- `FileChooser::set_files` targets the input that opened the chooser; Motus's call named no
  element. Downloads go to a per-context directory through `Browser.setDownloadBehavior`
  (`allowAndName`), so `path`/`save_as` return the file; in Motus the path was never
  recorded.

- `agito install --revision` takes a version, a milestone, a Chromium revision or a release
  channel, and looks the build up. A revision shared by a branch's patch releases installs
  the newest of them. Motus put the value straight into the download URL, so
  only an exact version worked, and the revision number its README suggested could not.
- `agito install` also downloads for Linux on Arm. A build is extracted into a staging
  directory and renamed into place, so an interrupted install never looks installed, and a
  failed one cleans up. Motus extracted in place and then trusted any directory under the
  build's name.
- `agito install --channel chrome|edge|firefox` fails with a message rather than exiting
  successfully without installing anything. There is no legacy `.installed` marker.
- `agito screenshot` and `agito pdf`:
  - The browser comes from `agito.config.json`, `AGITO_*` and the install marker, as in a
    test. Motus read only the marker.
  - They also take a local file or a bare host name (http for localhost, https otherwise).
  - They hide scrollbars, so captures have no scrollbar strip and a full-page shot is the
    viewport's width, not the content's.
  - The screenshot format follows the output's extension, and `--quality` applies to JPEG
    and WebP. `--delay` takes fractions of a second.
  - pdf adds `--landscape` and `--background`.
  - They warn when the page answers with an HTTP error, and fail when it cannot load.
- `agito shard merge` reads JUnit only (TRX is not ported) and can also write HTML. It
  merges suites by binary across shards, reports a test found in two shards, and keeps the
  worse of its results. An empty shard writes an empty suite carrying its shard number, so
  `--expect` does not count it missing.
- `update-protocol` is `cargo xtask update-protocol`, since it rewrites the bindings in this
  repository. It verifies the package's sha512 before using it. Its diff also lists types, and
  commands, events and types whose definition changed, not only added and removed commands
  and events. It records the vendored version in `protocol/VERSION`, regenerates the bindings
  and runs `cargo check`. Motus only wrote the JSON, to the current directory by default.
- `agito run`:
  - It builds with cargo and runs the test binaries itself, so it reports plain `#[test]`s
    as well as `#[agito::test]`s, but not doctests.
  - Test names are `binary::test`, such as `agito::popups::closes`, for filters, quarantine
    lists and flaky history. Filters go after `--` and match substrings, as libtest's do.
  - `--retries` re-runs any failure, and a pass on a retry is flaky: Motus's `flake` policy.
    Its default `transient` policy, re-running only lost browsers, is what the harness
    already does inside every test, so there is no `--retry-policy`.
  - The console report is always shown; `--reporter` adds JUnit or HTML files. JUnit has a
    `<testsuite>` per binary, and the shard in `agito.shard.*` properties.
  - A crashed binary's unreported tests are re-run alone to find the one that crashed.
  - Under plain `cargo test`, a quarantined `#[agito::test]` that fails prints a warning and
    passes.
- Accessibility audits:
  - Contrast is judged against the background actually behind the text: the element's and its
    ancestors' backgrounds, blended over the white canvas. Motus read the element's own
    `background-color`, and its colour parser dropped the alpha channel, so the transparent
    default read as black and most text on most pages failed. Text on a background image or
    gradient is skipped, as is text in a disabled control.
  - Every frame is audited, cross-origin iframes included; Motus audited the main document
    only. The page-wide rules (language, main landmark) apply to the top document only.
  - A rule sees a whole document and returns its violations, so page-wide rules need no
    first-node trick, and each duplicate id is a violation of its own. Violations carry a CSS
    selector and their frame's URL; Motus's carried no selector.
  - A plugin rule with a built-in rule's id replaces it.
  - The unlabeled-form-control rule also covers `searchbox`.
  - The audit hook fails a test in enforce mode from the test harness, under plain
    `cargo test`; Motus's needed its CLI runner. It is configured per context
    (`ContextOptions::accessibility`), so one test can differ, and keeps each violation once.
    It also audits after double clicks and checkboxes when auditing after actions.
- `to_have_role` and `to_have_accessible_name` read the element's own accessibility node.
  Motus took the first node in the element's subtree that the tree does not ignore, so a
  wrapper that is itself ignored reported a descendant's role and name. agito also accepts
  ARIA's spellings (`img`, `presentation`) where Chrome uses its own, and adds
  `to_have_accessible_description`.
- `filter_has` and `filter_has_not` apply. Motus accepted `Has` and `HasNot` in its locator
  options but never used them, so they matched everything. agito also adds
  `filter_has_not_text`.
- The protocol log keeps Motus's line format and redaction rules. It is also set by
  `launch.protocolLog` in the config file, and read with the other settings rather than on
  every launch. Several processes share a file through append mode, which is atomic per
  write, instead of Motus's cross-process mutex, which .NET's file appends needed.
- Storage state:
  - The file is Playwright's `storageState` JSON, not Motus's own serialization, and keeps each
    cookie's `SameSite` policy, which Motus dropped.
  - Local storage is restored by a page on each origin. Motus ran `localStorage.setItem` on
    the context's first page while it was still `about:blank`, so the values never reached
    their origin.
  - Only pages' own origins are saved: a cross-site iframe's storage is partitioned under the
    site embedding it, so it cannot be restored as that origin's.
- `--hide-banners` removes known consent managers by id, and anything else named for
  cookies, consent, GDPR or privacy only through its outermost fixed or sticky container.
  Motus removed every element whose class or id held one of those words (or `cc-`), which
  took out footer links, articles, and the whole page when `<body>` matched. It also cleared
  scroll locks even when it removed nothing.

- MCP server:
  - Tool calls run one at a time. Motus gated only finding the active page, assuming the
    protocol delivers calls one at a time; it does not, and the server runs a client's
    concurrent calls together, so a click could reach the page before the snapshot that
    named its ref.
  - The session holds its tabs itself, since an agito context holds pages weakly.
  - `agito mcp` drives Chromium-based browsers only; Motus also launched Firefox, where
    snapshots were unavailable.
  - Each tab keeps its own console and network logs from the moment it opens; Motus logged
    only the active tab, from when it became active.
  - Frames are matched to the iframe elements hosting them through the browser's record of
    each frame's owner. Motus compared window indexes, and when those did not settle it, took
    the first frame not yet claimed.
  - A field's typed text is folded into its value; Motus printed it twice
    (`textbox "Card" [value="4242"]: 4242`).
  - An action that navigates the page retires the refs in hand, and using one says the page
    navigated; Motus kept them, so they were resolved against the new document.
  - `audit_accessibility` audits frames too, naming each violation's `frameUrl`, and its text
    lists the violations rather than only counting them.
  - A snapshot says it found nothing addressable when nothing besides the document itself
    took a ref. Motus required no refs at all, but Chrome reports the document as focusable,
    so it always takes one and the note could not appear.
  - `evaluate` also takes statements, returning the last one's value.
  - The routing tools install one route per context, consulted for every rule, so the
    newest-rule-wins order is the table's own rather than the engine's.
  - Not yet: the `recording` group, which needs tracing and video.
  - Not yet: `get_performance` and `generate_pom`, which wait for performance metrics and
    code generation in agito itself.

## Inherited as-is

- `receives_events` accepts a hit on any descendant of the target's parent, so an element
  directly under `<body>` is never reported as covered.

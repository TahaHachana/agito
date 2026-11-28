# Changelog

## Unreleased

### Breaking changes
- `ContextOptions` has new fields, `storage_state` and `accessibility`, and `LaunchOptions`
  and `ConnectOptions` have a new field, `protocol_log`. Code that builds them with
  `..Default::default()`, as the docs do, is unaffected.

### Added
- `agito mcp`, a Model Context Protocol server (the new `agito-mcp` crate) that lets AI agents
  drive a browser: navigation, accessibility snapshots with refs that the interaction tools
  take, clicks, typing, keys, selects, checkboxes, waits, `evaluate`, screenshots and tabs. It
  starts its browser on first use and starts another if it crashes. `file://` URLs are
  blocked unless `--allow-unrestricted-file-access` is passed.
- `Page::locator_by_backend_node_id` and `Frame::locator_by_backend_node_id`, and the
  `backend-node-id=` selector, for the element an accessibility node names.
- Automatic accessibility audits. With `accessibility.mode` set to `warn` or `enforce` in
  `agito.config.json` (or `AGITO_ACCESSIBILITY_MODE`, or `ContextOptions::accessibility`),
  each context audits after every navigation, and optionally after actions that change a
  page. In enforce mode, `#[agito::test]` fails a test that leaves violations behind, under
  plain `cargo test`. `context.accessibility_violations()` returns what was found.
  `agito run --a11y warn|enforce` sets the mode for a run and reports violations in the
  console, JUnit and HTML.
- Accessibility audits. `page.audit_accessibility` checks a page and its iframes against nine
  WCAG-based rules (image alt text, labels on form controls, button and link names, colour
  contrast, duplicate ids, the document's language, heading levels and a main landmark), and
  `expect(&page).to_pass_accessibility_audit()` fails on what it finds, naming each element
  by a CSS selector. Contrast is measured against the background actually behind the text.
  Plugins can add rules with `register_accessibility_rule`, or replace a built-in one by id.
  `page.accessibility_snapshot` returns the accessibility tree itself.
- `to_have_role`, `to_have_accessible_name` and `to_have_accessible_description` assert what
  assistive technology sees, from the browser's accessibility tree: the computed role
  (implicit roles included, with ARIA's spellings such as `img` and `presentation`), the
  accessible name and the accessible description. Names and descriptions take text or a
  regex.
- The protocol log: every message agito sends to the browser and every one it gets back, a
  line each, with timings, sessions and commands that never got an answer. Turn it on with
  `AGITO_PROTOCOL_LOG=<file or stderr>`, `launch.protocolLog` in `agito.config.json`, or
  `LaunchOptions::protocol_log` and `ConnectOptions::protocol_log`, which also take a
  callback. Secrets (cookie values, authorization and token headers, passwords, request
  bodies) are replaced and long strings trimmed. Several processes can share one log file.
- `testing::FakeSocket::enqueue_raw` delivers bytes that need not be valid JSON.
- `Locator::filter_has` and `filter_has_not` keep the matches that contain, or do not
  contain, an element another locator matches, with any kind of selector, inside shadow roots
  too. `filter_has_not_text` is the opposite of `filter_has_text`.
- Storage state, to start contexts already signed in. `context.storage_state()` reads a
  context's cookies (with their `SameSite` policy) and the local storage of each page's
  origin; `StorageState::save` and `load` keep it as JSON in Playwright's `storageState`
  format. `ContextOptions::storage_state` starts a context from it, and
  `context.set_storage_state` applies one to an existing context. Local storage is written by
  a temporary page that visits each origin through a route, so nothing goes to the network.

### Fixed
- `fill` and `clear` work on `contenteditable` elements. They passed the editable check but
  set a `value` such elements do not have, so the element did not change. They now select
  the element's contents and replace them as if typed, so the page sees its input events.

### Deprecated
- `ErrorKind::AmbiguousSelector`. It was never raised: an action acts on the first element
  its locator matches. It will be removed in 0.4.

## 0.3.0 (2026-11-27)

The first release of the `agito` command-line tool (`agito-cli`) and of
[the agito book](https://tahahachana.github.io/agito/).

### Breaking changes

Found with `cargo semver-checks` against 0.2.0:

- `agito-protocol` follows a newer `devtools-protocol` (see Changed): some types, fields and
  enum variants Chrome removed are gone, protocol structs gained fields, and enums gained
  variants. Build protocol structs with `..Default::default()` to stay clear of new fields.
- `agito::transport::TransportError` has a new variant, `SessionClosed`, for a command whose
  page closed before the browser answered.
- `agito::harness::TestOptions` has a new field, `quarantine`. `#[agito::test]` fills it in;
  only code building `TestOptions` by hand needs to set it.

### Added
- The agito book (`book/`), published to GitHub Pages: how agito is built, a guide to
  writing and running tests, and a reference for the CLI, configuration and errors. Its
  examples are tests in `samples/agito-samples`, so they compile and pass in CI.
- API documentation: every public item in `agito` is documented (`missing_docs` is now a
  warning, and CI fails on warnings), the crate page gives an overview and links to the
  book, and the main types have examples that compile as doctests. The crates link to the
  book as their homepage.
- The `agito` command-line tool (crate `agito-cli`), starting with `agito install`. It
  downloads Chrome for Testing into `~/.agito/browsers`, where the launcher finds it with no
  configuration. `--revision` picks the build by version (`131.0.6778.85`), milestone
  (`131`), Chromium revision (`1368529`) or channel (`stable`, `beta`, `dev`, `canary`).
  `--path` installs elsewhere.
- `agito screenshot` and `agito pdf` capture a page in a fresh headless browser. They take a
  URL, a local HTML file or a bare host name, with `--wait-until`, `--timeout`, `--delay` and
  `--hide-banners`. Screenshots pick PNG, JPEG or WebP from the output's extension and take
  `--full-page`, `--width`, `--height` and `--quality`. PDFs take `--width`, `--landscape` and
  `--background`.
- `agito run` builds the tests with cargo and runs each test binary itself. It reports plain
  `#[test]`s and `#[agito::test]`s alike, with `--shard`, `--workers`, `--retries` (a pass
  on a retry is reported as flaky), `--fail-on-flaky`, `--quarantine <file>`,
  `--flaky-history`, and JUnit and HTML reports. A crashed binary's unfinished tests are
  re-run alone to find the one that crashed.
- `agito shard merge` combines the JUnit reports of a sharded run into one summary and report
  (JUnit or HTML). `--expect N` fails the merge when a shard is missing or appears twice.
- `#[agito::test(quarantine = "reason")]`: the test runs, but a failure is reported rather
  than failing it.
- With `AGITO_RESULTS_DIR` set, each `#[agito::test]` writes a JSON record of its outcome
  (`agito::harness::TestRecord`): duration, attempts, message, screenshot and quarantine.
- `cargo xtask update-protocol [--version <npm version>] [--dry-run]` vendors a
  `devtools-protocol` release, shows what changed, regenerates the bindings and checks that
  the workspace builds. `crates/agito-protocol/protocol/VERSION` records the vendored
  release.
- Uploads:
  - `locator.set_input_files` takes paths or in-memory `FilePayload`s, works on hidden
    inputs, and clears the input when given `InputFiles::none()`.
  - File choosers: `page.wait_for_file_chooser` and `page.on_file_chooser` return a
    `FileChooser` with `set_files`, `element()` and `is_multiple()`. File inputs no longer
    open native pickers.
- Downloads: `page.wait_for_download` and `page.on_download` return a `Download` with `path`,
  `save_as`, `failure`, `cancel`, `delete`, `progress`, `url` and `suggested_filename`.
  Each context downloads into its own temporary directory, removed when the context closes.
- Popups and pages opened by pages (`window.open`, `target=_blank`, `noopener`):
  - `page.wait_for_popup`, `page.on_popup` and `page.opener()`.
  - `context.wait_for_page` and `context.on_page`, which fire for every page in the
    context, including those from `new_page`.
  - A popup gets the context's viewport, emulation, init scripts and routes before it runs
    anything, including its first request.

### Changed
- The CDP bindings follow `devtools-protocol` 0.0.1756183 (from 0.0.1596832), adding the
  `Ads`, `CrashReportContext`, `DigitalCredentials` and `FindInPage` domains. Chrome removed
  86 definitions in that span, which `agito-protocol` drops too, a breaking change for code
  using them: among them the Privacy Sandbox enrollment commands, the Storage interest-group
  and shared-storage events and types, several Audits issue types, and
  `Network.InterceptionId`.
- New pages attach through browser-level auto-attach and start paused, so agito sets them up
  before they load anything. Tabs in contexts agito does not own are left running and
  detached.

### Fixed
- `#[agito::test]` re-runs a test whose browser went away during it, however the test
  failed. Before, only a test that returned the browser-lost error was re-run: one that
  asserted on an error it expected (`unwrap_err()` on a click meant to time out) saw the
  disconnect instead and panicked, which failed it. Chrome itself sometimes crashes during a
  run (seen with Chrome for Testing 158), which made `actionability_failures_say_why` flaky.
- A command still waiting for its answer when its page closed waited out the full 60s
  command timeout, because Chrome does not answer commands on a target it is destroying. A
  click on a button that closes its own window sometimes did this. Such commands now fail at
  once with a target-closed error when the page's session detaches.
- Clicks in out-of-process iframes that had to be scrolled into view sometimes missed. Chrome
  routes input to such frames through the compositor, which can lag a frame behind a scroll.
  agito now waits for the page to render after scrolling, then re-checks before clicking.
- `evaluate::<()>` no longer asks the browser to serialize the result. Values like a
  `Window` cannot be serialized and used to fail.

## 0.2.0 (2026-11-27)

### Added
- Network:
  - `Request` and `Response` objects: headers, post data, resource type, frame, failure,
    the request behind a response, and `body()`, `text()` and `json()`.
  - `page.on_request`, `on_response`, `on_request_finished` and `on_request_failed`.
  - `page.wait_for_request` and `page.wait_for_response` start listening when called, so
    an action started afterwards cannot outrun them.
  - `page.wait_for_network_idle`.
  - Routing with `page.route` / `context.route` (glob, regex, predicate or substring) to
    `fulfill`, `continue_` or `abort` requests, plus `unroute` and `unroute_all`. This works
    for documents and inside out-of-process iframes.
  - HAR 1.2 recording with `page.start_har`, `stop_har` and `stop_har_to`.
- Iframe support, same-origin and cross-origin (out-of-process):
  - `Frame`: `page.main_frame()`, `page.frames()`, `page.frame(name)`, `page.frame_by_url(..)`.
    A frame has `goto`, `evaluate`, `evaluate_with`, `content`, `set_content`, `title`,
    `wait_for_load_state`, `wait_for_url`, `locator` and `get_by_*`, plus `parent_frame`,
    `child_frames`, `is_detached` and `is_out_of_process`.
  - `FrameLocator`: `page.frame_locator(sel)`, `frame.frame_locator(sel)` and
    `locator.content_frame()`. These find the iframe on every use; nested `frame_locator`,
    `first`, `last` and `nth` are supported.
  - Clicks, hovers and `bounding_box` inside frames use page coordinates, and iframes are
    scrolled into view before acting in them.
  - Console messages, page errors and dialogs from out-of-process iframes reach the page's
    handlers.

### Fixed
- Actions on a page stalled for about 5s after another page was opened in the same context.
  The older page became a hidden background tab that does not render, so input sent to it
  waited for a frame. Each page now opens in its own window.
- `evaluate::<()>` no longer fails when the expression returns a value; decoding into `()`
  discards it.
- Chromium failed to launch on CI runners and other machines that cannot start its sandbox,
  such as Ubuntu 23.10+ with a browser outside its packaged install path. Launch errors that
  mention the sandbox now say what to do.

### Changed
- Chromium runs without its sandbox by default (`--no-sandbox`), as in Playwright. Set
  `LaunchOptions::chromium_sandbox`, `launch.chromiumSandbox` or `AGITO_CHROMIUM_SANDBOX=1`
  to keep it.
- `Error`'s `Debug` output is the error message; use `{:#?}` for the structured form.

### Changed (breaking)
- `Response` is now a handle with accessor methods (`status()`, `url()`, `headers()`,
  `header()`, `ok()`, ...) instead of public fields. `Response::synthetic` builds one for
  tests.
- `SelectorStrategy::resolve` takes `frame: &Frame` instead of `page: &Page`;
  `selectors::eval_to_handles` likewise.

## 0.1.0

First release: Chromium launcher and connection, pages, locators with actionability checks,
`expect` assertions, `#[agito::test]`, plugins and selector strategies.

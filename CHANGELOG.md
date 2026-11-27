# Changelog

## Unreleased

### Added
- The `agito` command-line tool (crate `agito-cli`), starting with `agito install`. It
  downloads Chrome for Testing into `~/.agito/browsers`, where the launcher finds it with no
  configuration. `--revision` picks the build by version (`131.0.6778.85`), milestone
  (`131`), Chromium revision (`1368529`) or channel (`stable`, `beta`, `dev`, `canary`).
  `--path` installs elsewhere.
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
- New pages attach through browser-level auto-attach and start paused, so agito sets them up
  before they load anything. Tabs in contexts agito does not own are left running and
  detached.

### Fixed
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

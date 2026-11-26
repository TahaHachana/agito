# Changelog

## 0.2.0 (unreleased)

### Added
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
- Chromium failed to launch on CI runners and other machines that cannot start its sandbox,
  such as Ubuntu 23.10+ with a browser outside its packaged install path. Launch errors that
  mention the sandbox now say what to do.

### Changed
- Chromium runs without its sandbox by default (`--no-sandbox`), as in Playwright. Set
  `LaunchOptions::chromium_sandbox`, `launch.chromiumSandbox` or `AGITO_CHROMIUM_SANDBOX=1`
  to keep it.
- `Error`'s `Debug` output is the error message; use `{:#?}` for the structured form.

### Changed (breaking)
- `SelectorStrategy::resolve` takes `frame: &Frame` instead of `page: &Page`;
  `selectors::eval_to_handles` likewise.

## 0.1.0

First release: Chromium launcher and connection, pages, locators with actionability checks,
`expect` assertions, `#[agito::test]`, plugins and selector strategies.

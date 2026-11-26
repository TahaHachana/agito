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

### Changed (breaking)
- `SelectorStrategy::resolve` takes `frame: &Frame` instead of `page: &Page`;
  `selectors::eval_to_handles` likewise.

## 0.1.0

First release: Chromium launcher and connection, pages, locators with actionability checks,
`expect` assertions, `#[agito::test]`, plugins and selector strategies.

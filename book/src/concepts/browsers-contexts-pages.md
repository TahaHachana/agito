# Browsers, contexts and pages

Five objects make up the API, each inside the one before it:

| Object | What it is | Made by |
|---|---|---|
| `Browser` | A running browser process, or a connection to one | `agito::launch`, `agito::connect` |
| `BrowserContext` | An isolated profile: cookies, storage, cache, routes, init scripts, emulation settings | `browser.new_context(options)` |
| `Page` | A tab | `context.new_page()`, or the page itself (`window.open`, `target=_blank`) |
| `Frame` | A document in a page: the main frame, or an iframe | `page.main_frame()`, `page.frames()` |
| `Locator` | A way to find elements, resolved each time it is used | `page.locator(...)`, `page.get_by_role(...)`, and so on |

In a test you are usually given a page, and reach the rest from it: `page.context()` is its
context and `page.context().browser()` its browser.

## Browser

A `Browser` is a launched browser or a connection to one that is already running. Dropping
the last handle to a browser agito launched closes it and removes its temporary profile;
`browser.close()` does the same explicitly. A browser agito connected to is left running.

Under `#[agito::test]` you rarely touch the browser: the harness launches one per test binary
and shares it.

## BrowserContext

A context is the unit of isolation. Two contexts in one browser share nothing a website can
see: not cookies, not local storage, not the cache. Opening a context is cheap (milliseconds,
unlike launching a browser), which is why every test gets its own.

A context's [`ContextOptions`](../writing/fixtures.md#context-options) apply to all its pages:
viewport, user agent, locale, timezone, colour scheme, extra HTTP headers, a base URL for
relative navigations, and default timeouts. Routes and init scripts set on a context apply to
every page in it, including ones opened later.

## Page

A page is a tab. Its `main_frame()` is the document it shows; `page.goto`,
`page.locator` and the like are shortcuts for the same calls on the main frame.

New pages start paused. Before a page runs any script or makes any request, agito applies
the context's viewport and emulation, init scripts and routes, then lets it go. This holds
for pages opened by pages too, so a popup's first request already sees your routes.

Each page gets a window of its own, so no page is ever a background tab, which browsers
throttle and do not render.

## Frame

Every document in a page is a `Frame`: the main frame and one per iframe, including
cross-origin iframes, which Chrome runs in a separate process. A `Frame` is just a page and
a frame id, so it is cheap and never holds the page alive. Most tests reach into iframes with
a [`FrameLocator`](../writing/frames-and-popups.md), which finds the iframe again on every use
and so survives it reloading.

## Locator and ElementHandle

A `Locator` describes how to find elements: a selector plus refinements such as "the first"
or "the one containing this text". It finds nothing until an action or assertion uses it, and
then looks again every time, so a locator made before the page re-rendered still works after.

An `ElementHandle` is a reference to one particular element. It goes stale when that element
is replaced. Use locators; handles are for the rare case that needs one specific node, such
as a [selector strategy](../extending.md).

## Lifetimes

All of these are cheap, `Clone` handles. Closing a context closes its pages; closing the
browser closes everything. Using a page after it closed fails at once with an error that
[`is_browser_lost()`](../reference/errors.md), rather than waiting for a timeout.

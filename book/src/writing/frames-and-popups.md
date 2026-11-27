# Frames and popups

## Iframes

Locators on a page do not look inside iframes. To act in one, locate the iframe with
`frame_locator`, then locate inside it:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:frames}}
```

A `FrameLocator` finds its iframe again every time it is used, so it keeps working if the
iframe reloads or is replaced. It has the same `locator` and `get_by_*` methods as a page,
`first()`, `last()` and `nth(i)` for when several iframes match, and `frame_locator` for an
iframe inside an iframe. `locator.content_frame()` turns a locator for an `<iframe>` element
into a frame locator.

This works the same for cross-origin iframes, which Chrome runs in a separate process.
Clicks inside them land in the right place, because agito works out each iframe's position
in the page up the whole chain of frames.

To work with a frame directly, as a `Frame` object:

```rust,ignore
for frame in page.frames() {
    println!("{} {}", frame.name(), frame.url());
}
let payment = page.frame_by_url("**/payment").expect("the payment frame");
let title: String = payment.evaluate("document.title").await?;
```

`page.frame(name)` finds a frame by its `name` attribute and `page.frame_by_url(pattern)` by
URL. A `Frame` has `goto`, `evaluate`, `content`, `locator` and the `get_by_*` methods, plus
`parent_frame()` and `child_frames()`.

## Popups and new tabs

A page opened by a page, through `window.open` or a link with `target=_blank`, is a popup:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:popups}}
```

As with network waiters, `wait_for_popup` listens from when it is created, so create it before
the click. `popup.opener()` is the page that opened it.

A popup belongs to its opener's context, so it shares its cookies, and gets the context's
viewport, emulation, init scripts and routes before it runs anything. `page.on_popup`
calls a closure for every popup a page opens; `context.on_page` and `context.wait_for_page`
see every new page in a context, popup or not.

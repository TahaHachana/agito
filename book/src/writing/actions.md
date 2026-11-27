# Actions

Actions are methods on a locator. Each one finds the element, waits until it can be used,
then acts. Clicks, `type_text` and `press` are real mouse and key events, as a user's would
be; `fill` sets an input's value directly, which is faster.

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:actions_form}}
```

## What each action waits for

Before acting, an action checks the element over and over until every check passes or the
timeout runs out. The checks depend on the action:

| Action | Waits until the element is |
|---|---|
| `click`, `dblclick`, `click_with`, `hover` | visible, enabled, stable (not moving), and not covered by another element; it is scrolled into view first |
| `fill`, `clear`, `type_text` | visible, enabled and editable |
| `press`, `select_option` | visible and enabled |
| `check`, `uncheck`, `set_checked` | visible and enabled, then as for a click |
| `focus`, `set_input_files`, `scroll_into_view` | attached to the page |

"Not covered" means a click at the element's centre would land on the element itself, not
on an overlay, a cookie banner or a modal backdrop. This is the most common reason a click
that a human would make is refused, and the error says which element is in the way.

## The actions

| Method | Does |
|---|---|
| `click()`, `dblclick()` | Clicks with the left button |
| `click_with(ClickOptions)` | Clicks with another button, click count or modifier keys |
| `hover()` | Moves the mouse over the element |
| `fill(text)` | Sets the value of an input or textarea, and fires its `input` and `change` events |
| `clear()` | Empties an input or textarea |
| `type_text(text)` | Types `text` key by key, for pages that react to each keystroke |
| `press(key)` | Presses a key or combination: `"Enter"`, `"Tab"`, `"Control+A"`, `"Shift+ArrowDown"` |
| `check()`, `uncheck()`, `set_checked(bool)` | Sets a checkbox or radio button, and fails if it did not change |
| `select_option(&[values])` | Selects options of a `<select>` by value or label, and returns what is selected |
| `set_input_files(files)` | Sets the files of a file input: see [Files](files-and-dialogs.md) |
| `focus()`, `scroll_into_view()` | What they say |

For input that is not about one element, `page.mouse()` and `page.keyboard()` give direct
control. The mouse can `move_to`, press and release buttons (`down`, `up`), `click`,
`dblclick` and scroll (`wheel`), at viewport coordinates. The keyboard can `press` a key or
combination, hold and release keys (`down`, `up`), `type_text` key by key and `insert_text`
in one go, into whatever has focus.

## Timeouts

Actions wait 30 seconds by default. Change it for one locator with `with_timeout`, for a
context with `action_timeout` in [`ContextOptions`](fixtures.md#context-options), or for
every test with `locator.timeout` in [`agito.config.json`](../reference/configuration.md):

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:actions_timeout}}
```

A timed-out action's error names the check that never passed and why, such as an element
that was found but is not visible, or one covered by another.

## Navigation

Navigation methods are on the page:

| Method | Does |
|---|---|
| `goto(url)` | Loads `url` and waits for its `load` event. Returns the document's response, or `None` for a same-document navigation. |
| `goto_with(url, NavigateOptions)` | The same, waiting for another state (`DomContentLoaded`, `NetworkIdle`, `Commit`) or with another timeout or a referer |
| `reload()`, `go_back()`, `go_forward()` | What they say |
| `wait_for_url(pattern, timeout)` | Waits until the URL matches, after a navigation a click started |
| `wait_for_load_state(state, timeout)` | Waits until the current document reaches a state |
| `set_content(html)` | Replaces the document with `html` |

A relative URL resolves against the context's `base_url`. Navigations time out after 30
seconds.

## Running JavaScript

```rust,ignore
let width: u32 = page.evaluate("window.innerWidth").await?;
page.evaluate::<()>("localStorage.clear()").await?;
let text: String = locator.evaluate("function () { return this.value; }", &[]).await?;
```

`page.evaluate` runs an expression in the page and turns its result into any type that
implements `serde::Deserialize`; a promise is awaited. `locator.evaluate` calls a function
with the element as `this`. `page.add_init_script(script)` runs a script in every document
the page loads, before the page's own scripts.

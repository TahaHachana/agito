# Writing a plugin

A plugin extends every browser context: it can teach locators a new kind of selector, and run
code around navigations, actions and page lifetimes. agito's own selectors are a plugin, loaded
first, through the same API.

This chapter builds a plugin that adds a `qa=` selector for elements with a `data-qa`
attribute, and records every URL a test visits. The code is in agito's repository, in
[`samples/agito-samples/tests/book_plugin.rs`](https://github.com/TahaHachana/agito/blob/main/samples/agito-samples/tests/book_plugin.rs),
where it runs as a test.

Plugin traits are async traits, so a plugin crate needs
[`async-trait`](https://crates.io/crates/async-trait):

```toml
[dependencies]
agito = "0.3"
async-trait = "0.1"
serde_json = "1"
```

## A selector strategy

A `SelectorStrategy` turns the part of a selector after its prefix into elements. Its `name`
is the prefix:

```rust,ignore
{{#include ../../samples/agito-samples/tests/book_plugin.rs:strategy}}
```

`resolve` gets the frame to search, and whether to search inside shadow roots. The easiest way
to find elements is `frame.query_all`, which evaluates a JavaScript expression that returns an
array of elements. Return them in document order.

A strategy can also implement `generate`, which makes a selector for a given element, and
`priority`, which orders strategies when generating one. Tools that write selectors use
these; resolving does not.

## A lifecycle hook

A `LifecycleHook` is called around things that happen to a page. Every method has a default
that does nothing, so implement only what you need:

```rust,ignore
{{#include ../../samples/agito-samples/tests/book_plugin.rs:hook}}
```

| Method | Called |
|---|---|
| `before_navigation(page, url)`, `after_navigation(page, response)` | Around `goto`, on a page or a frame. `reload`, `go_back` and `go_forward` do not call them. |
| `before_action(page, action)`, `after_action(page, result)` | Around every locator action; `result` says whether it failed |
| `on_page_created(page)` | When any page opens in the context, popups included |
| `on_page_closed(page)` | When `page.close()` closes a page; not when its context closes, or the page closes itself |

A hook's error is logged and otherwise ignored: a hook never fails a test.

## The plugin

A `Plugin` has a unique `id`, and registers what it provides in `on_loaded`, which runs once
for every new browser context:

```rust,ignore
{{#include ../../samples/agito-samples/tests/book_plugin.rs:plugin}}
```

## Loading it

There are two ways:

- **`#[agito::plugin]`**, as above, registers the type when the program is linked. Every
  context in that binary loads it, with no code at the call site. The type must implement
  `Default`. In tests, this means every test in the binary the plugin is compiled into: put a
  plugin meant for all your tests in a shared library crate.
- **`LaunchOptions::plugins`** passes plugins to one browser explicitly:
  `LaunchOptions { plugins: vec![Arc::new(QaPlugin)], ..Default::default() }`.

Either way, a plugin whose `id` is already loaded is skipped, and one whose `on_loaded` fails
is skipped with a warning.

## Using it

Once loaded, the new prefix works anywhere a selector does:

```rust,ignore
{{#include ../../samples/agito-samples/tests/book_plugin.rs:use}}
```

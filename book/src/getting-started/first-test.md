# Your first test

Create `tests/shop.rs` in your crate:

```rust,ignore
use agito::{expect, Page};

{{#include ../../../samples/agito-samples/tests/book.rs:first_test}}
```

Run it like any other test:

```sh
cargo test --test shop
```

## What happened

`#[agito::test]` turned the async function into an ordinary `#[test]`. When it ran:

1. The test binary launched a browser, the first time any of its tests needed one. Every
   test in the binary shares it.
2. The test got a new **browser context**, an isolated profile with its own cookies,
   storage and routes, and a **page** in it. That is the `page: Page` parameter.
3. `page.goto` loaded the page and waited for its `load` event.
4. `get_by_label("Search")` made a **locator**: a description of how to find the search box,
   not the box itself. `fill` found it, waited until it could be typed into, and typed.
5. `click` did the same for the button, which submitted the form.
6. The two `expect` assertions checked the URL and the result, retrying until they held or
   their timeout passed.
7. The context was closed, and with it the page.

Had the test failed, `cargo test` would show the error; a timed-out action, for instance,
says which check the element failed (visible, enabled, not covered) and on which selector.

## The return type

A test returns `()` or a `Result`. With `agito::Result<()>`, the `?` operator works on every
agito call and an error fails the test with its message. Panics fail it too, so `assert!`
and friends work as usual.

## Next

Read [Architecture](../concepts/architecture.md) for how the pieces fit together, or jump
to [Locators](../writing/locators.md) to start writing tests.

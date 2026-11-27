# Page objects

As a suite grows, the same selectors turn up in many tests, and a change to the page means
changing all of them. A page object puts what a page offers in one place: a struct that holds
the `Page`, with a method for each element tests use and each thing users do there.

This one, from agito's samples, describes a todo app:

```rust,ignore
{{#include ../../../samples/agito-samples/src/lib.rs:page_object}}
```

Tests then read as what they check. Here `todo(&page)` is a small helper in the same file that
loads the app and wraps the page in a `TodoAppPage`:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/page_object_model.rs:page_object_test}}
```

Some habits that keep page objects useful:

- **Return locators, not elements.** A method such as `add_button()` returns a `Locator`, so
  every use finds the element afresh and waits for it. Tests can then act on it or `expect`
  it.
- **Name things as users see them,** and prefer `get_by_role`, `get_by_label` and
  `get_by_test_id`, so the page object survives a redesign.
- **Put actions in, keep assertions out.** `add_todo` is a step a user takes; whether the
  count went up is for the test to check.
- **Share them in a library.** Integration tests in `tests/` each compile to their own
  binary, so shared code goes in the crate's `src/` (as here, in `agito-samples`) or in a
  `tests/common/mod.rs` that each test file declares with `mod common;`.

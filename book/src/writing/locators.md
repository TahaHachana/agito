# Locators

A locator describes how to find elements. Making one finds nothing: it is resolved when an
action or assertion uses it, and again on every use, so it keeps working while the page
changes underneath it.

## Finding elements the way users do

Prefer locators that describe what a user sees over how the page is built. They read better
and survive markup changes:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:locators_get_by}}
```

| Method | Finds |
|---|---|
| `get_by_role(role, name)` | Elements with an ARIA role, such as `button` or `link`, and optionally an accessible name. It reads the browser's accessibility tree, so implicit roles count: a plain `<button>` has the role `button`. |
| `get_by_label(text)` | Form controls by their label: a `<label for>`, a wrapping `<label>`, `aria-labelledby` or `aria-label` |
| `get_by_placeholder(text)` | Inputs by placeholder |
| `get_by_text(text)` | Elements whose text contains `text`, ignoring case |
| `get_by_text_exact(text)` | Elements whose whole text is `text` |
| `get_by_title(text)` | Elements by `title` attribute |
| `get_by_alt_text(text)` | Images by `alt` text |
| `get_by_test_id(id)` | Elements by `data-testid` |

These exist on `Page`, `Frame` and `FrameLocator`.

## Selectors

`page.locator(selector)` takes a selector string. Without a prefix it is CSS; a
`prefix=` picks another strategy:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:locators_selectors}}
```

| Prefix | Meaning |
|---|---|
| (none) or `css=` | A CSS selector |
| `xpath=` | An XPath expression |
| `text=` | The innermost elements containing the text |
| `role=` | An ARIA role, with an optional name: `role=button[name="Save"]` |
| `data-testid=` | The `data-testid` attribute |

CSS and text selectors search inside open shadow roots by default;
`locator.pierce_shadow(false)` turns that off.
[Plugins](../extending.md) can add prefixes of their own.

## Narrowing down

Refinements apply in the order you write them:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:locators_refine}}
```

- `first()`, `last()` and `nth(i)` pick one match (`nth` counts from 0).
- `filter_has_text(text)` keeps matches whose text contains `text`, case-sensitively.
- `locator(selector)` searches inside each match with a CSS selector. A selector starting
  with `..` steps up to the parent first: `locator.locator("..")` is the parent itself. It
  returns a `Result`, which is an error only for an empty selector.

### Filtering by what an element contains

To keep the matches that contain something, or that do not, filter them with another
locator:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:locators_filter}}
```

- `filter_has(&locator)` keeps matches that contain an element the other locator matches;
  `filter_has_not(&locator)` keeps those that contain none. The other locator can use any
  kind of selector, `get_by_*` included, and finds elements inside shadow roots too. An
  element does not count as containing itself.
- `filter_has_not_text(text)` is the opposite of `filter_has_text`.

The other locator is resolved across the page, then each match is checked for containing one
of its elements, so it must be on the same page and in the same frame. Its own refinements
apply across the page: `filter_has(&page.locator("button").first())` keeps the match that
contains the page's first button, not each match's first button.

An action acts on the first element the locator matches, so when several match, narrow it
down with the methods above until it means the one you want. Assertions such as
`to_have_count` look at every match.

## Reading from the page

Locators also read:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:locators_read}}
```

`text_content`, `inner_text`, `inner_html`, `get_attribute`, `input_value` and
`bounding_box` read one element; `count`, `all`, `all_text_contents` and `all_inner_texts`
read every match. `is_visible`, `is_enabled`, `is_checked` and the like answer at once,
without waiting. To wait for an element to reach a state, use `wait_for` or, better, an
[assertion](assertions.md).

# Assertions

`expect` checks something about a locator, a page or a response. Assertions on locators and
pages retry until they hold, so they wait for the page to catch up without a sleep:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:assertions}}
```

An assertion checks every 100ms until it holds or its timeout passes, 10 seconds by default.
When it fails, the error says what was expected, what was last seen, the selector and the
page's URL.

## On a locator

| Assertion | Holds when |
|---|---|
| `to_be_visible()`, `to_be_hidden()` | The element is (or is not) visible. A missing element is hidden. |
| `to_be_enabled()`, `to_be_disabled()` | The element is (or is not) enabled |
| `to_be_editable()` | The element is an enabled, writable input, textarea, `contenteditable` or `role=textbox` |
| `to_be_checked()` | A checkbox or radio button is checked |
| `to_be_empty()` | An input, textarea or select has no value; any other element has no text but whitespace |
| `to_be_focused()` | The element has focus |
| `to_be_attached()`, `to_be_detached()` | The element is (or is not) in the document |
| `to_have_text(text)` | The element's whole text is `text`, or matches a regex |
| `to_contain_text(text)` | The element's text contains `text` |
| `to_have_value(value)` | An input's value is `value`, or matches a regex |
| `to_have_attribute(name, value)` | An attribute is `value`, or matches a regex |
| `to_have_class(name)` | The element has the class `name` |
| `to_have_css(property, value)` | A computed style property has the value `value` |
| `to_have_count(n)` | The locator matches `n` elements |
| `to_have_role(role)` | The element's computed ARIA role is `role` |
| `to_have_accessible_name(name)` | Its accessible name is `name`, or matches a regex |
| `to_have_accessible_description(text)` | Its accessible description is `text`, or matches a regex |

Text assertions compare the element's `textContent` exactly, spaces included. A locator that
matches several elements is checked on the first; use `to_have_count` for how many there are.

## Roles and accessible names

Three assertions check an element the way assistive technology sees it, read from the
browser's accessibility tree:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:assertions_accessibility}}
```

- **The role** is the computed one, so elements have their implicit roles without a `role`
  attribute: a `<nav>` is a `navigation`, an `<input>` a `textbox`, a `<button>` a `button`.
  ARIA's spellings work: `img` for an image, and `none` or `presentation` for an element
  hidden from assistive technology.
- **The accessible name** is what a screen reader announces: from a `<label>`, `aria-label`,
  `aria-labelledby`, an image's alt text, or the element's own text for roles that take
  their name from it, such as buttons, links and headings. An element with no name has the
  name `""`, which catches an icon-only button nobody can identify by ear.
- **The accessible description** is the extra text a screen reader reads after the name,
  usually from `aria-describedby`.

## On a page

| Assertion | Holds when |
|---|---|
| `to_have_title(title)` | The title is `title`, or matches a regex |
| `to_have_url(pattern)` | The URL matches a glob or a regex |

A URL glob uses `*` for anything within a path segment and `**` for anything across
segments: `**/search?q=*` matches `https://shop.test/search?q=lamp`. A pattern without `*`
must equal the whole URL.

## On a response

`expect(&response).to_be_ok()` and `to_have_status(code)` check a response from `goto` or
the [network API](network.md). A response does not change, so these check once and are not
awaited.

## Regular expressions

Anything that takes text also takes a `regex::Regex` (add the `regex` crate to your
dev-dependencies):

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:assertions_regex}}
```

## Negating, timeouts and messages

`not()` inverts an assertion: `expect(&locator).not().to_be_visible()` waits until the
element is hidden. `with_timeout` sets the timeout for one assertion, and `with_message`
replaces the failure message:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:assertions_options}}
```

To change the default timeout everywhere, set `assertions.timeout` in
[`agito.config.json`](../reference/configuration.md) or `AGITO_ASSERTIONS_TIMEOUT`.

## Assertions or reads?

`locator.inner_text()` and the like read the page once. Asserting on what they return with
`assert_eq!` does not wait, so it fails if the page has not caught up yet. Use `expect`
whenever the page might still be changing, which in a browser is almost always.

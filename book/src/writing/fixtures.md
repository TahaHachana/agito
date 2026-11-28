# Fixtures and context options

## Fixtures

A test asks for what it needs through its parameters. Any of these, in any order:

| Parameter | What it is |
|---|---|
| `page: Page` | A new page, in the test's own context |
| `context: BrowserContext` | That context |
| `browser: Browser` | The browser the test binary shares |

A test that needs a second page opens it in its context, where it shares cookies and routes
with the first:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:fixtures}}
```

## Context options

`ContextOptions` decides what the pages in a context look like to a website. To give one test
different options, write a function that returns them and name it in the attribute:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:context_options}}
```

Start from `ContextOptions::default()`, which already holds the values from
[`agito.config.json`](../reference/configuration.md) and `AGITO_*` variables, and change
what the test needs:

| Field | Default | Sets |
|---|---|---|
| `viewport` | 1280x720 | The window's size in CSS pixels; `None` keeps the browser's own |
| `user_agent` | the browser's | The `User-Agent` header and `navigator.userAgent` |
| `locale` | the browser's | `navigator.language` and `Accept-Language`, such as `fr-FR` |
| `timezone_id` | the machine's | The timezone, such as `Europe/Paris` |
| `color_scheme` | none | What `prefers-color-scheme` reports: `Light`, `Dark` or `NoPreference` |
| `ignore_https_errors` | `false` | Accept invalid TLS certificates, for test servers |
| `base_url` | none | What relative URLs in `goto` resolve against |
| `extra_http_headers` | none | Headers added to every request |
| `proxy_server` | none | A proxy for the context's requests, such as `http://proxy:3128` |
| `action_timeout` | 30s | The default timeout for [actions](actions.md#timeouts) |
| `navigation_timeout` | 30s | The default timeout for navigations |
| `storage_state` | none | Cookies and local storage to start with: see [Signing in once](#signing-in-once) |

Outside tests, the same options go to `browser.new_context(options)` or
`browser.new_page(options)`.

## Cookies and init scripts

A context also holds cookies and init scripts:

```rust,ignore
use agito::Cookie;

context.add_cookies(&[Cookie::new("session", "abc123", "https://shop.test")]).await?;
let cookies = context.cookies().await?;
context.clear_cookies().await?;

// Runs in every document of every page in the context, before the page's own scripts.
context.add_init_script("window.__TEST__ = true;").await?;
```

Setting cookies in a test, or in a helper the tests share, is the quickest way to start a
test already signed in.

## Signing in once

Signing in through the user interface in every test is slow, and makes every test depend on
the sign-in page. Instead, sign in once, save the context's **storage state** (its cookies and
local storage), and start other contexts from it:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:storage_state}}
```

`context.storage_state()` reads every cookie in the context, and the local storage of the
origin each of its pages is on. `save` and `StorageState::load` write and read it as JSON in
Playwright's `storageState` format, so state files work across tools. A context starts from
one through `ContextOptions::storage_state`, or takes one later with
`context.set_storage_state(&state)`.

To start a whole suite signed in, load the state in a [context options](#context-options)
function:

```rust,ignore
use agito::storage::StorageState;
use agito::ContextOptions;

fn signed_in() -> ContextOptions {
    let state = StorageState::load("target/agito/signed-in.json")
        .expect("run the sign-in step first: cargo test --test sign_in");
    ContextOptions {
        storage_state: Some(state),
        ..ContextOptions::default()
    }
}

#[agito::test(context = signed_in)]
async fn orders_show_the_last_purchase(page: Page) -> agito::Result<()> {
    page.goto("https://shop.test/orders").await?;
    // ...
    Ok(())
}
```

and produce the file in a step that runs before the tests, such as a test binary of its own
(`tests/sign_in.rs`) that signs in and saves the state, run with `cargo test --test sign_in`
ahead of the rest.

Some things to know:

- **A state file holds live credentials.** Keep it out of version control, in `target/` or
  another ignored directory.
- **Local storage needs a page to write it.** Starting from a state with local storage briefly
  opens one page in the new context, which visits each origin through a route that answers
  with an empty document (nothing goes to the network). Lifecycle hooks see that page.
- **A cross-site iframe's storage is not saved.** Chrome partitions it by the site embedding
  it, so it is not the storage that origin has on its own, and cannot be set again the same
  way. Cookies are saved whoever set them.
- **Session storage is not saved,** as it belongs to a single tab.

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

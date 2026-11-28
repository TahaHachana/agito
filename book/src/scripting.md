# Scripting a browser

agito is not only for tests. Outside `#[agito::test]` you launch the browser yourself, inside
your own tokio runtime. This example, from agito's repository, saves a screenshot of a page:

```rust,ignore
{{#include ../../crates/agito/examples/screenshot.rs}}
```

Its dependencies are agito and tokio:

```toml
[dependencies]
agito = "0.3"
tokio = { version = "1", features = ["full"] }
```

## Launching

`agito::launch(LaunchOptions::default())` starts a browser. The defaults come from
[`agito.config.json`](reference/configuration.md) and `AGITO_*` variables; change any of
them in code:

| Field | Default | Sets |
|---|---|---|
| `headless` | `true` | Whether the browser has a window |
| `channel` | none | Which installed browser to use: `Chrome`, `Edge` or `Chromium` |
| `executable_path` | none | A browser executable to use instead |
| `args` | none | Extra command-line switches |
| `ignore_default_args` | none | agito's default switches to leave out |
| `slow_mo` | 0 | A pause before every command |
| `timeout` | 30s | How long launching may take |
| `user_data_dir` | a temporary profile | A profile directory to keep between runs |
| `downloads_path` | none | Where downloads go |
| `chromium_sandbox` | `false` | Run Chromium with its sandbox |
| `plugins` | none | [Plugins](extending.md) to load into every context |
| `protocol_log` | none | Log every protocol message: see [the protocol log](writing/flaky-tests.md#the-protocol-log) |

The browser closes when `browser.close()` is called or the last handle to it is dropped, and
its temporary profile is removed.

`browser.new_page(options)` opens a page in a new context. For several pages that share
cookies, open a context with `browser.new_context(options)` and pages in it with
`context.new_page()`.

## Connecting to a running browser

`agito::connect` attaches to a browser started elsewhere, with `--remote-debugging-port`:

```rust,ignore
let browser = agito::connect("http://127.0.0.1:9222", agito::ConnectOptions::default()).await?;
let page = browser.new_page(Default::default()).await?;
// ...
browser.disconnect().await?; // leaves the browser running
```

It takes the `http://host:port` address or a `ws://` endpoint. `new_page` and `new_context`
work in contexts agito creates, isolated from what the browser already had open.

To drive what is already open instead, such as a signed-in profile, adopt the browser's own
default context:

```rust,ignore
let context = browser.default_context(Default::default()).await?;
let tab = &context.pages()[0]; // the tabs already open, as they are
// ...
context.close().await?; // lets go of the context; its windows stay open
```

Nothing is emulated on an adopted context's pages (viewport, user agent, locale and the like
are left as the browser has them), its downloads are not redirected, and closing it lets go of
it rather than closing anything.

## From the command line

For one-off captures there is no need to write code:

```sh
agito screenshot example.com -o page.png --full-page --hide-banners
agito pdf report.html -o report.pdf --background
```

Both take a URL, a local HTML file or a bare host name, and wait for the page to load.
`--hide-banners` removes cookie-consent dialogs first. See the [command
reference](reference/cli.md#agito-screenshot) for every option.

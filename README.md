# agito

Browser automation and testing for Rust. agito drives Chromium over the Chrome DevTools
Protocol directly (a pipe to the browser it launches, or a WebSocket to one already running),
with no driver process or Node.js sidecar in between. Its own selector strategies are
registered through the same plugin API third-party code uses.

agito is a Rust port of Motus, a .NET browser automation framework. It is early (0.2):
Chromium-based browsers only. See
[docs/parity.md](https://github.com/TahaHachana/agito/blob/main/docs/parity.md) for what
has been ported so far and [docs/roadmap.md](https://github.com/TahaHachana/agito/blob/main/docs/roadmap.md)
for what comes next.

## A test

```rust
use agito::{expect, Page};

#[agito::test]
async fn search_returns_results(page: Page) -> agito::Result<()> {
    page.goto("https://example.com").await?;

    page.locator("input[name='q']").fill("agito automation").await?;
    page.get_by_role("button", Some("Search")).click().await?;

    expect(&page).to_have_url("**/search**").await?;
    expect(&page.locator(".results")).to_be_visible().await?;
    Ok(())
}
```

`#[agito::test]` runs under plain `cargo test`:

- **Fixtures:** parameters of type `Page`, `BrowserContext` and `Browser` are filled in.
- **Isolation:** each test gets a fresh context and page, in a browser shared by the whole test
  binary.
- **Retries:** a test whose browser disconnects underneath it is re-run once
  (`#[agito::test(retries = N)]`). Failed assertions are never re-run.

Actions wait for the element to be attached, visible, enabled, stable and not covered.
`expect` assertions re-check until they pass or time out. Defaults are 30s for actions and
10s for assertions.

## Iframes

Same-origin and cross-origin (out-of-process) iframes work the same way:

```rust
// Found afresh each time, so it survives the iframe reloading.
let checkout = page.frame_locator("iframe#payment");
checkout.get_by_label("Card number").fill("4242 4242 4242 4242").await?;
checkout.get_by_role("button", Some("Pay")).click().await?;
expect(&checkout.locator(".status")).to_have_text("Paid").await?;

// Or work with frame objects directly.
for frame in page.frames() {
    println!("{} {}", frame.name(), frame.url());
}
let ads = page.frame("ads").unwrap();
let title: String = ads.evaluate("document.title").await?;
```

## Network

```rust
use agito::FulfillOptions;
use serde_json::json;

// Mock an API: matching requests never reach the network.
page.route("**/api/user", |route| async move {
    let _ = route.fulfill(FulfillOptions::json(&json!({ "name": "Ada" }))).await;
}).await?;

// Wait for a response triggered by an action. The waiter listens from the moment it is
// created, so create it first.
let response = page.wait_for_response("**/api/search*", None);
page.get_by_role("button", Some("Search")).click().await?;
let results: serde_json::Value = response.await?.json().await?;

page.on_request(|r| println!("{} {}", r.method(), r.url()));
page.wait_for_network_idle(std::time::Duration::from_millis(500), None).await?;

page.start_har();
// ...
page.stop_har_to("session.har")?;
```

Routes can also be set on a `BrowserContext`, where they apply to every page in it. A route
can fulfill a request, continue it (optionally changing the URL, method, headers or body), or
abort it. A handler that does none of these lets the request through unchanged.

## Popups

```rust
let popup = page.wait_for_popup(None);           // listening starts here
page.get_by_text("Open report").click().await?;
let popup = popup.await?;
popup.wait_for_load_state(agito::WaitUntil::Load, None).await?;
expect(&popup.locator("h1")).to_have_text("Report").await?;
```

Pages opened by pages, from `window.open` or `target=_blank` links, get the context's
viewport, init scripts and routes before they run. `context.on_page` and
`context.wait_for_page` see every new page in a context.

## Files

```rust
use agito::FilePayload;

page.locator("input[type=file]").set_input_files(["report.pdf"]).await?;
page.locator("#avatar").set_input_files(FilePayload::new("me.png", png_bytes)).await?;

let download = page.wait_for_download(None);
page.get_by_text("Export CSV").click().await?;
download.await?.save_as("export.csv").await?;
```

## Scripts

```rust
let browser = agito::launch(agito::LaunchOptions::default()).await?;
let page = browser.new_page(Default::default()).await?;
page.goto("https://example.com").await?;
let png = page.screenshot(Default::default()).await?;
browser.close().await?;
```

## Installing a browser

agito launches an installed Chrome, Edge or Chromium. To download one for agito instead:

```bash
cargo install agito-cli
agito install                         # the current stable Chrome for Testing
agito install --revision 131          # the newest build of a milestone
agito install --revision 131.0.6778.85
```

Builds go to `~/.agito/browsers`, and the launcher prefers the last one installed there.

The same tool captures pages:

```bash
agito screenshot example.com -o page.png --full-page --hide-banners
agito pdf report.html -o report.pdf --background
```

## Configuration

Settings come from three places, each overriding the one before:

1. `agito.config.json`, searched for upward from the working directory.
2. `AGITO_*` environment variables.
3. Options passed in code.

```json
{
  "launch": { "headless": true, "channel": "chrome", "slowMo": 0, "executablePath": null, "chromiumSandbox": false },
  "context": { "viewport": { "width": 1280, "height": 720 }, "locale": "en-US", "baseURL": "http://localhost:3000" },
  "locator": { "timeout": 30000 },
  "assertions": { "timeout": 10000 },
  "failure": { "screenshot": true, "screenshotPath": "target/agito-failures" }
}
```

Variables: `AGITO_HEADLESS`, `AGITO_CHANNEL`, `AGITO_SLOWMO`, `AGITO_LAUNCH_TIMEOUT`,
`AGITO_EXECUTABLE_PATH`, `AGITO_CHROMIUM_SANDBOX`, `AGITO_LOCALE`, `AGITO_COLOR_SCHEME`, `AGITO_IGNORE_HTTPS_ERRORS`,
`AGITO_BASE_URL`, `AGITO_LOCATOR_TIMEOUT`, `AGITO_ASSERTIONS_TIMEOUT`,
`AGITO_FAILURES_SCREENSHOT`, `AGITO_FAILURES_SCREENSHOT_PATH`, `AGITO_MAX_CONTEXTS`.

## Workspace

| Crate | What it holds |
|---|---|
| `crates/agito` | The engine: launcher, transport, browser/context/page, locators, input, selectors, plugins, `expect`, test harness |
| `crates/agito-cli` | The `agito` command-line tool: `install`, `screenshot`, `pdf` |
| `crates/agito-macros` | `#[agito::test]` and `#[agito::plugin]` |
| `crates/agito-protocol` | CDP bindings generated from the vendored protocol JSON |
| `samples/agito-samples` | Example page object and tests, ported from Motus.Samples |
| `xtask` | Developer tasks: `cargo xtask codegen`, `cargo xtask update-protocol` |

## Development

```bash
cargo test --workspace                     # unit tests, no browser needed
cargo test -p agito -p agito-samples -p agito-cli \
  --features agito/integration,agito-samples/integration,agito-cli/integration   # drives a real Chrome/Chromium
cargo xtask codegen [--check]              # regenerate (or verify) the CDP bindings
cargo xtask update-protocol [--dry-run]    # vendor the latest devtools-protocol and regenerate
cargo run -p agito --example screenshot -- https://example.com shot.png
```

## License

MIT

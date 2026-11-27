# Installation

## Add agito to your project

Browser tests usually live in a crate's `tests/` directory, so agito is a dev-dependency:

```toml
[dev-dependencies]
agito = "0.3"
```

agito needs a recent stable Rust (the minimum is the `rust-version` in its `Cargo.toml`) and
runs on Linux, macOS and Windows.

agito runs its own [tokio](https://tokio.rs) runtime for `#[agito::test]`, so a test crate
does not need tokio or a runtime attribute. Two crates come up often enough to add now:
`serde_json`, for JSON in network mocks and responses, and `regex`, for matching text against
a pattern.

## Get a browser

agito launches the first browser it finds, in this order:

1. `AGITO_EXECUTABLE_PATH`, or `executablePath` in [`agito.config.json`](../reference/configuration.md).
2. The browser `agito install` downloaded most recently.
3. Chrome, then Edge, then Chromium, where their installers put them.

So if Chrome is installed, agito will use it. To pin a version instead, which keeps CI and
every developer on the same build, install the agito command-line tool and let it download
[Chrome for Testing](https://developer.chrome.com/blog/chrome-for-testing):

```sh
cargo install agito-cli
agito install                  # the current stable build
agito install --revision 131   # the newest build of a milestone
```

Builds go to `~/.agito/browsers`, where the launcher finds them with no configuration. See
[`agito install`](../reference/cli.md#agito-install) for every option.

## Headless or not

Browsers run headless by default. To watch a test run, turn that off for one run:

```sh
AGITO_HEADLESS=0 cargo test
```

`AGITO_SLOWMO=250` adds a 250ms pause before every command the browser receives, which
makes the steps easy to follow.

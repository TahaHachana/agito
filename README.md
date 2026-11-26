# agito

Extensible browser automation and testing for Rust. agito drives Chromium over the Chrome
DevTools Protocol and Firefox over WebDriver BiDi directly, with no driver process or Node.js
sidecar in between.

agito is a Rust port of [Motus](../Motus), the .NET framework. The port is in progress; see
[docs/parity.md](docs/parity.md) for what has landed.

## Workspace

| Crate | What it holds |
|---|---|
| `crates/agito-protocol` | CDP bindings generated from the vendored protocol JSON (`cargo xtask codegen`) |
| `crates/agito` | The engine: transport, sessions, and (as phases land) browser, page, locators, plugins, assertions |
| `xtask` | Developer tasks: protocol code generation |

## Development

```bash
cargo test --workspace                 # unit tests, no browser needed
cargo xtask codegen                    # regenerate CDP bindings after updating protocol/*.json
cargo xtask codegen --check            # CI: fail if the bindings are stale
AGITO_CHROME=$(which google-chrome) cargo test -p agito --test live_transport -- --ignored
```

## License

MIT

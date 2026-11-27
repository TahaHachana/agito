# Roadmap: porting Motus to Rust

agito is a port of Motus, a .NET browser automation and testing framework of about 54k lines.
Motus drives Chromium over the Chrome DevTools Protocol (CDP) and Firefox over WebDriver BiDi,
with no driver process in between. Every built-in feature (selector strategies, lifecycle
hooks, wait conditions, reporters) registers through the same plugin API third parties use.

The goal is a full-parity port, delivered in phases, written as idiomatic Rust rather than
translated line by line. This document records the decisions and the order of work.
[parity.md](parity.md) tracks the details: what maps to what, known gaps, and deliberate
behavior differences.

## Decisions

- **Scope:** full parity, in phases. Each phase ends green in CI.
- **Test integration:** an `#[agito::test]` attribute that runs under plain `cargo test`. The
  Motus MSTest, NUnit and xUnit adapters are not ported.
- **Analyzers:** the Roslyn analyzers are not ported. Rust covers the important ones:
  - Futures are `#[must_use]`, so an unawaited call warns (MOT001).
  - `Locator` is `#[must_use]`, so an unused locator warns (MOT005).
  - Dropping the last `Browser` handle kills a browser agito launched (MOT004).
  - The rest (fixed sleeps, fragile selectors, legacy prefixes, unwaited navigations) are
    documentation, or a later optional lint.
- **Protocols:** CDP and BiDi, as in Motus. Chromium first; Firefox is phase 5.
- **Recorder and codegen output:** Rust code.
- **Configuration:** agito's own names only: `agito.config.json` and `AGITO_*` variables, with
  no fallback to Motus's files.
- **Toolchain:** edition 2024, a recent stable Rust (`rust-version` in `Cargo.toml`).
- **License:** MIT, as Motus.

## Architecture as built

1. **CDP is the internal language.** The engine speaks CDP method names through a `Session`
   trait: `send_raw`, plus a typed `send` and `subscribe` on `dyn Session`. Firefox will plug
   in behind the same trait through a CDP-to-BiDi translation layer, as in Motus.
   `Capabilities` plus `capabilities::require*` give clear errors for features a transport
   lacks.
2. **Generated protocol types are actually used.** `cargo xtask codegen` turns the vendored
   `browser_protocol.json` and `js_protocol.json` into checked-in serde types
   (`crates/agito-protocol/src/cdp`). Each command implements `Command` and each event
   implements `Event`. Motus generated bindings too, but the engine never used them; it
   hand-wrote about 237 records instead. CI fails when the bindings are stale.
   `protocol/VERSION` records the `devtools-protocol` npm release vendored, and
   `cargo xtask update-protocol` moves to another.
3. **Concrete handles, no abstractions crate.**
   - `Browser`, `BrowserContext`, `Page`, `Frame`, `Locator`, `ElementHandle`, `Request`,
     `Response`, `Route`, `Download` and `FileChooser` are cheap, `Arc`-backed handles.
   - `Frame` is just a page and a frame id; the page owns the frame table, so there are no
     reference cycles.
   - Plugin traits (`Plugin`, `SelectorStrategy`, `LifecycleHook`) are object-safe
     `async_trait`s. The built-in selectors register through the same `PluginContext`.
4. **Concurrency.**
   - Everything runs on tokio. Each connection has one receive loop.
   - Pending commands wait on a oneshot channel, with a 60s timeout. Once the connection
     drops, later commands fail at once.
   - Each event subscription gets its own unbounded channel. One channel can serve several
     event names, which keeps their wire order.
   - `#[agito::test]` runs every test on one process-wide runtime, so a browser shared by a
     test binary outlives any single test.
5. **New pages start paused.**
   - Browser-level `Target.setAutoAttach` with `waitForDebuggerOnStart` attaches every new page
     before it runs, whether it came from `new_page` or a popup.
   - agito applies the context's emulation, init scripts and routes in one ordered batch that
     ends by resuming the page.
   - Each page opens in its own window, so no page is ever a hidden background tab, which
     would not render.
6. **Frames.**
   - Out-of-process iframes are auto-attached per page, and their sessions are tracked
     alongside the page's own.
   - Pointer actions compute page coordinates from each `<iframe>`'s content box up the chain.
   - `FrameLocator` finds its iframe again on every use, so it survives reloads.
7. **Errors.** A single `agito::Error` wraps an `ErrorKind` that mirrors Motus's exception
   hierarchy. `Error::is_browser_lost()` is the one check the test harness retries on.
8. **Plugin discovery.** `#[agito::plugin]` registers a plugin at link time through
   `inventory`. Plugins can also be passed explicitly in `LaunchOptions::plugins`.
9. **Injected JavaScript.** The page scripts live in `crates/agito/js/*.js`, loaded with
   `include_str!`. They are ported from Motus with fixes, which parity.md lists.
10. **Launching.**
    - On Unix the launcher uses `--remote-debugging-pipe`, with fds 3 and 4 wired through
      `/bin/sh` rather than unsafe code, so the browser dies with the process. Elsewhere it
      uses a WebSocket port.
    - Chromium runs with `--no-sandbox` by default, as Playwright does. CI runners, containers
      and downloaded browsers often cannot start its sandbox.
    - Temporary profiles are removed on close. Profiles left by crashed test binaries are swept
      on the next launch.

## Workspace

```
crates/
  agito-protocol/   generated CDP types (+ BiDi types in phase 5)
  agito/            the engine: transport, launch, browser/context/page/frame, locator,
                    selectors, input, network, files, plugins, expect, config, harness
  agito-macros/     #[agito::test], #[agito::plugin]
  agito-cli/        the `agito` binary: install, screenshot, pdf, run
samples/
  agito-samples/    page objects and tests ported from Motus.Samples
xtask/              cargo xtask codegen [--check], cargo xtask update-protocol
scripts/publish.sh  release: checks, package, publish in order, tag
```

Planned crates: `agito-recorder`, `agito-mcp` and `agito-runner`.

The test harness lives in `agito::harness`, not in a separate `agito-test` crate as first
planned. That way the attribute can be used as `#[agito::test]`.

## Phases

| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, CI, protocol codegen | Done (0.1.0) |
| 1 | Transport: sockets, CDP connection, sessions, capabilities, fake socket | Done; protocol log pending |
| 2 | Launch and connect, browser/context/page, frames, input, handles, config | Done for Chromium |
| 3 | Plugins, selectors, locators, network/routing/HAR, dialogs, popups, uploads, downloads | Done |
| 4 | `expect`, `#[agito::test]`, samples | Done; smaller gaps remain |
| 5 | Firefox over WebDriver BiDi | Next major engine phase |
| 6 | Accessibility, performance, coverage, tracing, video | Not started |
| 7 | CLI | In progress: `install`, `screenshot`, `pdf`, `update-protocol`, `run` done; `shard merge` left |
| 8 | Recorder, codegen, check-selectors | Not started |
| 9 | MCP server | Not started |
| 10 | Visual runner | Not started |

Releases: 0.1.0 covered phases 0-4 (MVP); 0.2.0 added iframes and network. Popups, uploads
and downloads are unreleased.

### Remaining in phases 1-4

- **Protocol log:** an optional file log of all browser traffic (Motus's `ProtocolLog`).
- **Locators and assertions:** `has` / `has_not` filters, and the accessible-name and role
  assertions.
- **Context state:** storage state save/load (to reuse a login), exposed bindings,
  permissions, geolocation and HTTP credentials.
- **Input:** touch, natural mouse motion, and the cursor overlay.
- **Browser management:** a browser pool, a heartbeat that notices frozen browsers, and
  signal handlers.
- **Connecting:** taking over tabs already open in a browser agito connects to.
- **Harness options:** `launch = fn`, `perf_budget(...)` and failure tracing.
- **Windows:** killing the shared test browser at process exit.

### Phase 5: Firefox / WebDriver BiDi

- Port `BiDiTransport` and `BiDiSession`.
- Port the CDP-to-BiDi translation registry (about 25 commands) and the event map.
- Port the script-value and key conversions.
- Port `FirefoxArgs`, the profile manager and the stderr endpoint reader.
- Port the Firefox-tagged integration tests.
- Features without a BiDi equivalent (the accessibility tree, coverage, tracing) report
  `NotSupported` through the capability guards.

### Phase 6: audits and artifacts

- **Accessibility:**
  - the accessibility-tree query and collectors
  - the WCAG contrast calculator
  - the rule engine with its nine rules
  - the audit lifecycle hook, with enforce/warn modes and skippable rules
- **Performance:** the metrics collector, budgets, and LCP/FCP/TTFB/CLS/INP assertions.
- **Coverage:** JS precise coverage and CSS rule usage, the aggregator and thresholds, and
  source-map remapping (VLQ decoder, parser, fetcher, remapper).
- **Tracing:** the zip packager, using Motus's layout: `trace.json`, `har.json`, screenshots.
- **Video:** screencast frames written by a port of the MJPEG/AVI writer.

### Phase 7: CLI (`agito`)

- **`install`:** done for Chrome for Testing. `--revision` takes a version, milestone,
  Chromium revision or channel. It writes the `~/.agito/browsers` markers the launcher reads.
  Installing Firefox comes with phase 5.
- **`screenshot` and `pdf`:** done.
- **`update-protocol`:** done, as `cargo xtask update-protocol` rather than an `agito`
  subcommand. It rewrites the bindings checked into this repository, so it only works in an
  agito checkout; the published CLI would have nothing to act on. It fetches a
  `devtools-protocol` release from npm, checks its sha512, shows what changed per domain
  (commands, events and types added, removed or redefined), regenerates the bindings and runs
  `cargo check`. `--dry-run` only shows the differences.
- **`run`:** first increment done.
  - It builds with `cargo test --no-run --message-format json`, then runs the test binaries
    itself, as cargo-nextest does, rather than running `cargo test`. That ties each result
    to its binary, lets a failed test be re-run alone, and makes a shard an exact list of
    tests. Binaries run with cargo's working directory, `CARGO_MANIFEST_DIR` and library
    path. Doctests are not run, as in nextest.
  - Results come from libtest's output, for every test. `#[agito::test]`s also write a JSON
    record when `AGITO_RESULTS_DIR` is set, which adds durations, messages, the failure
    screenshot and quarantine.
  - When a binary dies, the tests it left unreported are re-run one at a time, so the crash
    is pinned on the test that caused it.
  - Done: filters, `--shard`, `--workers`, `--retries` (a pass on a retry is flaky),
    `--quarantine <file>` and `#[agito::test(quarantine = "reason")]`, `--fail-on-flaky`,
    `--flaky-history`, and console, JUnit and HTML reports.
  - Left: Cobertura and coverage, accessibility and performance modes (all phase 6), and
    `run` settings in `agito.config.json`.
- **`shard merge`.**
- Motus's TRX reporter is not ported, since TRX is a .NET format.

### Phase 8: recorder and codegen

- Ports from Motus:
  - action capture: the injected script, the binding channel and the input state machine
  - selector inference, using the registered strategies in priority order
  - page analysis
- **`agito record`** emits an `#[agito::test]` function.
- **`agito codegen`** emits a page-object struct with `Locator` accessors and form helpers.
- Both write a selector manifest with DOM fingerprints.
- **`check-selectors`** finds selector literals in Rust sources with `syn`, checks them
  against live pages, and fixes them by span-based rewrites.

### Phase 9: MCP server

- `agito-mcp`, built on `rmcp`.
- Ports every Motus tool and the opt-in `--caps` groups: coordinates, recording, contexts,
  routing.
- Ports the accessibility snapshot format exactly: ref rules, folding, frame-prefixed refs.
- Ports the session, dialog, console and network services and the security policy.
- Transports:
  - stdio
  - streamable HTTP, with a browser per MCP session and a bearer token. It refuses a
    non-loopback address without a token.
- `agito mcp` takes the same flags as `motus mcp`.

### Phase 10: visual runner

- An axum server with WebSocket and embedded static assets, replacing Motus's Blazor UI.
- Views:
  - a test explorer (from `cargo test -- --list`)
  - a live browser view, streamed from tests
  - a timeline and step debugger
  - console and coverage panels
  - visual regression (pixel diff)
  - trace viewer, JUnit viewer, and interactive selector repair

## Testing

- **Unit tests** run against `agito::testing::FakeSocket` (feature `testing`), with no
  browser.
- **Integration tests** drive a real Chrome or Chromium behind the `integration` feature:

  ```
  cargo test -p agito -p agito-samples -p agito-cli \
    --features agito/integration,agito-samples/integration,agito-cli/integration
  ```

  Pages come from `set_content`, `data:` URLs, and a small local HTTP server in
  `crates/agito/tests/common`. Cross-origin iframes use `localhost` against `127.0.0.1`,
  which Chrome treats as different sites.
- **CI** runs formatting, clippy, the codegen check and unit tests on Linux, macOS and
  Windows, then the browser tests on Linux.
- **Not ported:** Motus's DLL-isolation fixtures, analyzer tests, and test-framework adapter
  tests.

## Verification per phase

- Unit tests, browser tests, clippy with `-D warnings`, and the codegen check all pass.
- New browser tests pass several runs in a row before merging. Flaky tests get a root cause,
  not a retry.
- parity.md is updated: the mapping table, gaps and behavior differences.
- Later phases add end-to-end checks:
  - `agito run` producing JUnit and HTML reports for the samples
  - `agito record` and `agito codegen` output that compiles and passes
  - `agito mcp` driven by an MCP client against fixture pages

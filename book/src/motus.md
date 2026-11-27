# Coming from Motus

agito is a Rust port of [Motus](https://github.com/DataficationSDK/Motus), a .NET browser
automation framework. It keeps Motus's design (CDP directly, everything built on a plugin
API) and most of its names, written as Rust rather than translated line by line.

## What maps to what

| Motus | agito |
|---|---|
| `MotusLauncher.LaunchAsync`, `ConnectAsync` | `agito::launch`, `agito::connect` |
| `IBrowser`, `IBrowserContext`, `IPage`, `IFrame`, `ILocator` | `Browser`, `BrowserContext`, `Page`, `Frame`, `Locator`: concrete types, not interfaces |
| `page.GetByRole("button", name: "Save")` | `page.get_by_role("button", Some("Save"))` |
| `await locator.ClickAsync()` | `locator.click().await?` |
| `Expect.That(locator).ToHaveTextAsync("x")` | `expect(&locator).to_have_text("x").await?` |
| `MotusTestBase`, `[MotusTestClass]` | `#[agito::test]` |
| MSTest, NUnit and xUnit adapters | Plain `cargo test` |
| `IPlugin`, `ISelectorStrategy`, `ILifecycleHook`, `[MotusPlugin]` | `Plugin`, `SelectorStrategy`, `LifecycleHook`, `#[agito::plugin]` |
| `motus.config.json`, `MOTUS_*` | `agito.config.json`, `AGITO_*` |
| `motus install`, `run`, `shard merge`, `screenshot`, `pdf` | `agito install`, `run`, `shard merge`, `screenshot`, `pdf` |
| `motus update-protocol` | `cargo xtask update-protocol`, in agito's own repository |

Motus's Roslyn analyzers are not ported: Rust covers the important ones. An unawaited future
and an unused locator are compiler warnings, and dropping the last `Browser` handle closes a
browser agito launched.

## Behaviour that differs

The full list is in [`docs/parity.md`](https://github.com/TahaHachana/agito/blob/main/docs/parity.md).
The ones most likely to matter:

- **Frame locators** (`page.frame_locator`) find their iframe on every use, so they survive it
  reloading. Motus had only frame objects, which go stale.
- **New pages start paused,** so a popup's first request already sees the context's routes,
  init scripts and emulation.
- **Locator refinements apply in the order written.** In Motus, `HasText` and `Nth` applied to
  the final set wherever they were written.
- **`set_input_files` works on hidden inputs,** and takes paths on disk as well as in-memory
  files.
- **`agito run` runs the test binaries cargo builds,** so it reports plain `#[test]`s too, and
  re-runs a single failed test. Its `--retries` is Motus's `flake` retry policy; the
  `transient` policy is what the test harness always does.
- **`agito install --revision`** takes a version, a milestone, a revision or a channel, and
  installs on Linux Arm too.

## Not ported yet

- **Firefox.** Motus drives it over WebDriver BiDi, but with large gaps of its own (no role
  locators, frames or MCP snapshots), so agito puts it off until there is demand.
- **Accessibility audits, performance budgets, coverage, tracing and video.**
- **The recorder, page-object generator and selector checker,** the **MCP server**, and the
  **visual runner.**
- Smaller gaps: storage state, exposed bindings, permissions and geolocation, touch input,
  and role and accessible-name assertions.

The [roadmap](https://github.com/TahaHachana/agito/blob/main/docs/roadmap.md) has the order of
work.

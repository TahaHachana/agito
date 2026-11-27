# Architecture

agito is a stack of layers. Each one only talks to the one below it, and you work almost
entirely with the top two.

```text
your tests        #[agito::test], run by agito::harness
                  │
the API you use   Browser ▸ BrowserContext ▸ Page ▸ Frame ▸ Locator
                  │  extended by plugins: selector strategies, hooks
                  │
sessions          Session: send a command, subscribe to events
                  │  typed by agito-protocol, generated from the CDP
                  │
the connection    CdpConnection: matches replies, routes events
                  │
the transport     a pipe (a browser agito launched, on Unix)
                  │  or a WebSocket (Windows, or connecting)
                  │
                  Chrome, Edge, Chromium, Chrome for Testing
```

## The crates

| Crate | What it holds |
|---|---|
| [`agito`](https://docs.rs/agito) | The engine: launching and connecting, the browser objects, locators and selectors, input, network, files, plugins, `expect`, the test harness |
| `agito-macros` | `#[agito::test]` and `#[agito::plugin]`, re-exported by `agito` |
| [`agito-protocol`](https://docs.rs/agito-protocol) | Rust types for the Chrome DevTools Protocol, generated from its JSON definitions, and re-exported as `agito::protocol` |
| `agito-cli` | The `agito` command: `install`, `run`, `shard merge`, `screenshot`, `pdf` |

You depend on `agito`. The CLI is a separate install.

## From the transport up

**The transport** carries JSON messages. When agito launches a browser on Linux or macOS, it
talks to it over a pipe (`--remote-debugging-pipe`), so the browser cannot outlive the
process that started it. On Windows, and for a browser that is already running, it uses a
WebSocket.

**The connection** sends commands and matches each reply to the command that asked for it.
It has one task that reads everything the browser sends: replies go to whoever is waiting
for them, and events go to whoever subscribed. A command that gets no answer fails after 60
seconds; when the connection drops, every waiting command fails at once, and so does one
waiting on a page that closes.

**Sessions** multiplex the connection. The browser has a session, and so does every page
and every out-of-process iframe. Everything above this layer talks through the `Session`
trait: send a typed command, get a typed reply, subscribe to events. The types come from
`agito-protocol`, which is generated from the protocol's JSON definitions, so a command's
parameters and reply are checked by the compiler.

**The browser objects** are what you use: `Browser`, `BrowserContext`, `Page`, `Frame`,
`Locator` and `ElementHandle`, described in the [next chapter](browsers-contexts-pages.md).
They are cheap handles: cloning one clones a pointer, and every clone refers to the same
thing.

**Plugins** extend the objects. A plugin registers selector strategies, which decide what a
selector such as `role=button` means, and lifecycle hooks, which run around navigations and
actions. agito's own selectors (CSS, XPath, text, role, test id and the `get_by_*` family)
are a built-in plugin registered through the same API. See [Writing a
plugin](../extending.md).

**The harness** is what `#[agito::test]` expands to. It owns the browser a test binary
shares, gives each test its fixtures, and decides when to re-run a test. See [How a test
runs](test-harness.md).

## Design choices

- **Chromium only, over CDP.** Talking to the browser directly means no driver binary to
  install or keep in step, and access to everything the protocol offers: network
  interception, input, emulation, downloads.
- **Concrete types.** There is no trait for "a page"; `Page` is a struct. The extension
  points are the plugin traits.
- **Locators, not element references.** A locator is resolved again every time it is used,
  so it survives the page re-rendering. Element handles exist, but tests rarely need them.
- **Waiting is built in.** Actions wait for elements to be usable and assertions retry, so
  tests do not need sleeps.
- **Async throughout,** on tokio.

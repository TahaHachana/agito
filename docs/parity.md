# Motus → agito parity

Tracks the port phase by phase. See the plan for design decisions.

| Phase | Scope | Status |
|---|---|---|
| 0 | Workspace, CI, CDP codegen (`xtask`), protocol crate | Done |
| 1 | Transport: socket halves, WebSocket + pipe sockets, CDP connection, sessions, registry, capabilities, fake socket | Done (protocol log pending) |
| 2 | Launcher, browser finder, Browser/Context/Page/Frame, handles, input, config | Not started |
| 3 | Plugins, selectors, Locator + actionability, network/routing/HAR, dialogs/downloads/file chooser | Not started |
| 4 | `expect` assertions, pool, failure capture, `agito-test` + `#[agito::test]`, examples | Not started |
| 5 | Firefox / WebDriver BiDi | Not started |
| 6 | Accessibility, performance, coverage, tracing, video | Not started |
| 7 | CLI (`agito`) | Not started |
| 8 | Recorder, codegen, check-selectors | Not started |
| 9 | MCP server | Not started |
| 10 | Visual runner | Not started |

## Phase 1 mapping

| Motus | agito |
|---|---|
| `ICdpSocket` | `transport::socket::{SocketSender, SocketReceiver, SocketPair}` |
| `CdpSocket` | `transport::ws::connect` |
| `CdpPipeSocket` | `transport::pipe::pipe_pair` |
| `CdpTransport` | `transport::CdpConnection` |
| `IMotusSession` / `CdpSession` | `transport::Session` / `CdpSession` (`send`, `events` on `dyn Session`) |
| `IMotusSessionRegistry` / `CdpSessionRegistry` | `transport::SessionRegistry` / `CdpSessionRegistry` |
| `MotusCapabilities` / `CapabilityGuard` | `transport::Capabilities`, `capabilities::require*` |
| `CdpProtocolException` / `CdpDisconnectedException` | `TransportError::{Protocol, Disconnected, Timeout}` |
| `Motus*Exception` hierarchy, `BrowserFailure.IsBrowserLost` | `Error` / `ErrorKind`, `Error::is_browser_lost` |
| `FakeCdpSocket` (tests) | `agito::testing::FakeSocket` (feature `testing`) |
| `CdpJsonContext.cs` (237 hand-written records) | `agito_protocol::cdp::*` (generated, all domains) |

Behavior differences, deliberate:
- An event key subscribed twice fans out to both subscribers (Motus's channels split events
  between competing readers), so the "already subscribed on its own" refusal is gone.
- Unknown CDP enum values decode to `UnrecognizedValue` instead of failing.

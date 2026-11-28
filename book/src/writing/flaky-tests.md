# Flaky tests

A flaky test passes and fails on the same code. In browser tests the usual causes are timing
(acting before the page is ready), shared state between tests, and the browser itself
misbehaving. agito removes the first two by design, and handles the third.

## What agito already does

- **Actions and assertions wait.** Actions wait until an element can be used, and assertions
  retry until they hold, so a test does not race the page. Avoid fixed sleeps
  (`page.wait_for_timeout`, `tokio::time::sleep`): wait for what you need instead, with an
  `expect`, `wait_for_url`, `wait_for_response` or `wait_for_network_idle`.
- **Every test has its own context,** so cookies, storage and routes never leak between
  tests.
- **A lost browser is retried.** If the browser crashes or disconnects during a test, the
  harness relaunches it and runs the test again. This happens once by default; raise it for a
  test that is exposed to it:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:retries}}
```

The harness never re-runs a test for failing an assertion or timing out: that is a real
failure until shown otherwise.

## Retrying failures with agito run

To re-run failed tests, and learn which ones pass on a second try, run the suite with
`agito run --retries`:

```sh
agito run --retries 2 --flaky-history target/flaky.json
```

A test that fails and then passes is reported as **flaky**, not as passed. With
`--fail-on-flaky` a flaky test fails the run, and `--flaky-history` keeps a count of each
test's runs, failures and flaky passes across runs, so the worst offenders stand out. See
[agito run](../running/agito-run.md#retries-and-flaky-tests).

## Quarantine

A test that is known to be flaky, and waiting for a fix, can be quarantined. It still runs,
and its result is still reported, but its failure does not fail the suite:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:quarantine}}
```

Under `cargo test` a quarantined test that fails prints the failure and its reason, then
passes. `agito run` reports it as quarantined. To quarantine tests without changing code, as
CI might for a test that started failing today, list them in a file for
[`agito run --quarantine`](../running/agito-run.md#quarantine).

Quarantine is a holding area, not a fix: give each one a reason that says where the fix is
tracked.

## Seeing what went wrong

- **Failure screenshots.** With `"failure": { "screenshot": true }` in
  [`agito.config.json`](../reference/configuration.md#failure), each failed test saves a
  screenshot of its page, to `target/agito-failures` by default. `agito run` puts it in its
  HTML report.
- **Console and page errors.** `page.on_console` and `page.on_page_error` show what the page
  logged and which of its scripts threw.
- **Watch it run.** `AGITO_HEADLESS=0 AGITO_SLOWMO=250 cargo test my_test` opens a visible
  browser and slows every step down.
- **Read the protocol log,** below, when the question is what agito and the browser actually
  said to each other.

## The protocol log

The protocol log records every message agito sends to the browser and every message it gets
back. It settles whether a problem is in agito, the page or the browser: a command that never
got an answer, an event that never came, or an error the browser returned.

```sh
AGITO_PROTOCOL_LOG=target/protocol.log cargo test checkout
```

`AGITO_PROTOCOL_LOG` takes a file or `stderr`; `launch.protocolLog` in
[`agito.config.json`](../reference/configuration.md#launch) does the same, and in code
`LaunchOptions::protocol_log` (or `ConnectOptions::protocol_log`) also takes a callback that
receives each line. Each line has a timestamp, a connection tag such as `c1`, and what
happened:

```text
2026-11-28T06:31:29.062Z c1 -> #53 DA67DB26... Page.navigate {"frameId":"7BC7...","url":"http://127.0.0.1:33413"}
2026-11-28T06:31:29.211Z c1 <- #53 DA67DB26... Page.navigate 149.3ms {"frameId":"7BC7...","loaderId":"..."}
2026-11-28T06:31:29.214Z c1 <- event DA67DB26... Page.loadEventFired {"timestamp":1234.5}
2026-11-28T06:31:29.385Z c1 xx #126 19800925... Runtime.evaluate cancelled after 124.7ms
```

`->` is a command and `<-` its answer, with how long it took, or `error <code> "message"`.
`<- event` is something the browser reported. `xx` is a command that never got an answer,
because it timed out or its page went away. The long hex string is the session: one per page
or iframe, and `-` for the browser itself.

Several processes can log to one file; each starts its part with a header, and each
connection's lines carry their own tag. Long strings are trimmed (`max_value_length`, 1024
characters by default), and secrets are replaced: cookie values, authorization, cookie and
token-like headers, raw header text, password fields and request bodies. What a test typed
and the scripts it ran are logged as they are, since hiding them would hide most of what the
log is for, so treat a log file as sensitive. On Unix it is created readable by its owner only.

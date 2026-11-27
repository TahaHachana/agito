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

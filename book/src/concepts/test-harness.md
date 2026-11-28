# How a test runs

`#[agito::test]` turns an async function into a `#[test]` that runs under `cargo test`, or
under [`agito run`](../running/agito-run.md). This chapter follows one test through.

## One browser per test binary

`cargo test` builds one binary per file in `tests/` (plus one for a crate's unit tests), and
runs the tests in each binary in parallel threads. agito launches one browser per binary, the
first time a test needs it, and every test in that binary shares it. Launching a browser
takes about a second; opening a context takes milliseconds.

All of a binary's tests run on one tokio runtime that lives as long as the process, so the
shared browser's background work never stops when a single test ends.

## Each test gets its own context and page

For each test the harness:

1. waits for a free context slot. At most four contexts are open at once per binary, however
   many threads libtest uses; set `AGITO_MAX_CONTEXTS` to change that.
2. opens a context with the test's [context options](../writing/fixtures.md), and a page in it.
3. runs the test body with the fixtures it asked for: any of `Page`, `BrowserContext` and
   `Browser`, as parameters in any order.
4. if the test failed, saves a screenshot of the page, when [failure
   screenshots](../reference/configuration.md#failure) are on.
5. closes the context.

Because every test has a fresh context, tests cannot leak cookies, storage or routes into
each other, and can run in parallel.

## When a test is re-run

A browser can go away under a test: Chrome can crash, or be killed. When that happens the
test's result says nothing about the code under test, so the harness relaunches the browser
and runs the test again, once by default (`#[agito::test(retries = 2)]` for more). This
covers a test that failed because the browser went away however it failed: by returning the
error, or by panicking on an assertion that saw the disconnect instead of what it expected.

A test that fails for any other reason, such as a failed assertion or a timeout, is never
re-run by the harness. To re-run failures in general, and report the ones that then pass as
flaky, use [`agito run --retries`](../running/agito-run.md#retries-and-flaky-tests).

## Quarantine

`#[agito::test(quarantine = "reason")]` still runs a test but does not let its failure fail
the suite: the harness prints the failure and the reason, and the test passes. `agito run`
reports it as quarantined rather than passed. See [Flaky tests](../writing/flaky-tests.md).

## What `agito run` reads

When `AGITO_RESULTS_DIR` is set, the harness writes a small JSON record for each test:
whether it passed, its duration, attempts, error message, failure screenshot and trace, and
quarantine.
`agito run` sets it and reads the records back for its reports. Under plain `cargo test`
nothing is written.

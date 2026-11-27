# Errors

Every fallible agito call returns `agito::Result<T>`, whose error is `agito::Error`. Its
message is written to be read in a test failure: a timed-out action names the check that
failed and the selector, and an assertion says what it expected and what it saw.

To act on an error in code, ask it a question or match its kind:

```rust,ignore
match locator.click().await {
    Err(e) if e.is_timeout() => { /* ... */ }
    Err(e) if e.is_browser_lost() => { /* ... */ }
    Err(e) => match e.kind() {
        agito::ErrorKind::Navigation { url, error_code, .. } => { /* ... */ }
        _ => { /* ... */ }
    },
    Ok(()) => {}
}
```

| Method | True for |
|---|---|
| `is_timeout()` | `NavigationTimeout`, `WaitTimeout`, `ActionTimeout` and `CommandTimeout` |
| `is_browser_lost()` | `TargetClosed`: the page, context or browser went away. The test harness re-runs a test that fails this way, or fails in any way while its browser disconnects. |

## Kinds

`ErrorKind` is non-exhaustive: match it with a `_` arm, since new kinds may be added.

| Kind | Raised when |
|---|---|
| `ActionTimeout` | An action's element never passed its checks. Carries the check that failed (`visible`, `enabled`, `editable`, `scrollIntoView`, `stable`, `receivesEvents`, or `resolve` for an element never found) and what was seen. |
| `NavigationTimeout` | A navigation did not reach its load state in time |
| `WaitTimeout` | A wait (for a popup, a download, a request, a URL, a state) did not finish in time |
| `CommandTimeout` | The browser did not answer a protocol command within 60 seconds |
| `Assertion` | An `expect` assertion failed. Carries the expected and actual values, the selector and the page URL. |
| `Navigation` | A navigation failed outright, such as `net::ERR_NAME_NOT_RESOLVED` |
| `ElementNotFound` | A selector matched nothing where an element was required at once |
| `TargetClosed` | The page, context, session or browser went away during the call |
| `Evaluation` | A script run in the page threw, or returned a value that could not be decoded |
| `Protocol` | The browser answered a command with an error |
| `InvalidArgument` | A call was given something that cannot work, such as an empty selector |
| `NotSupported` | The feature is not available with this browser connection |
| `Io` | Launching the browser or reading a file failed |
| `Other` | Anything else, with a message |

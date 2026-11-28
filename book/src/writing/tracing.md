# Tracing

A trace records what a context did, so you can see afterwards why a test went the way it did:
every action and navigation, the console, the network, and screenshots along the way.

```rust,ignore
use agito::TracingOptions;

context.start_tracing(TracingOptions::default()).await?;
page.goto("https://shop.test/").await?;
page.locator("#q").fill("shoes").await?;
page.locator("text=Search").click().await?;
context.stop_tracing("target/traces/search.zip").await?;
```

`discard_tracing` stops without saving. A context records one trace at a time; each context
records its own, so tests running in parallel trace at once.

## What a trace holds

The trace is a zip:

| File | Holds |
|---|---|
| `trace.json` | The event log: each event with its `time` in milliseconds from the start of the trace |
| `har.json` | Every request the context's pages made, in [HAR](https://w3c.github.io/web-performance/specs/HAR/Overview.html) format, each entry's `pageref` naming its page |
| `resources/screenshots/000001.jpeg`, ... | A screenshot after each action and navigation |
| `chrome-trace.json` | The browser's performance timeline, when asked for |

The events in `trace.json`:

| `type` | Fields |
|---|---|
| `action` | `action` (`click`, `fill`, ...), `target` (the locator), `duration`, `error`, `screenshot` |
| `navigation` | `url`, `status`, `duration`, `screenshot` |
| `console` | `level` (`log`, `error`, ...), `text` |
| `pageError` | `text`: an uncaught exception, with its stack |
| `dialog` | `kind` (`alert`, `confirm`, ...), `message` |
| `page` | `event` (`opened` or `closed`), `url` |
| `trace` | `title`, when the trace was given one |

Every event names its `page` (`page-1`, `page-2`, ...), and the log lists them in `pages`.
Tracing only watches: dialogs are handled as they would be without it.

## Options

```rust,ignore
TracingOptions {
    title: Some("checkout".into()), // written into trace.json
    screenshots: false,             // skip the screenshots (default: true)
    timeline: true,                 // add chrome-trace.json (default: false)
}
```

**`timeline`** records the browser's performance timeline (scripting, layout, painting),
which Chrome DevTools' Performance panel and [Perfetto](https://ui.perfetto.dev) open. Chrome
records one timeline at a time for the whole browser, so it covers every context, and a
second context asking for one waits until the first stops. That is why it is off by default.

## Keeping the traces of failed tests

Turn on `failure.trace` in [`agito.config.json`](../reference/configuration.md#failure), or
set `AGITO_FAILURES_TRACE=true`, and every `#[agito::test]` is traced, keeping the trace of
each one that fails:

```json
{ "failure": { "trace": true, "tracePath": "target/agito-failures" } }
```

```text
agito: failure trace saved to target/agito-failures/checkout__pays_by_card.trace.zip
```

[`agito run`](../running/agito-run.md) lists the trace under the failed test, attaches it in
the JUnit report and links it in the HTML report. Traces of passing tests are discarded.

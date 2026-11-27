# Network

agito can answer a page's requests itself, wait for particular ones, and watch them all.

## Mocking with routes

`page.route(pattern, handler)` sends every request whose URL matches `pattern` to `handler`
instead of the network. The handler decides what happens to it:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:network_mock}}
```

A handler gets the request as `route.request()` and settles it one of three ways:

| Call | Effect |
|---|---|
| `route.fulfill(FulfillOptions)` | Answers with a response you make: `FulfillOptions::json(&value)`, `::html(text)`, `::text(text)`, then `.with_status(404)` or `.with_header(name, value)` as needed. It can also take a body from a file (`path`). |
| `route.continue_(ContinueOptions)` | Lets the request go on, optionally with another URL, method, headers or body |
| `route.abort(None)` | Fails it, as a network error would. `Some("connectionrefused")`, `Some("timedout")` and the like pick the error. |

A handler that does none of these lets the request through unchanged.

Patterns are globs: `*` matches anything within a path segment and `**` anything across
segments. A pattern without `*` matches any URL that contains it. A `regex::Regex` works too.

When several routes match, the one added last wins, and a page's routes come before its
context's. That makes layering easy: a context route can serve a whole fake site, which is
how this book's examples work, while one test overrides a single endpoint with a page route,
as above. `page.unroute(id)` removes a route, with the id `route` returned, and
`page.unroute_all()` removes them all.

`context.route(pattern, handler)` adds a route for every page in a context, including pages
opened later and popups, which see it from their first request.

## Waiting for a request or response

`page.wait_for_response(pattern, timeout)` waits for a response whose URL matches:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:network_wait}}
```

The waiter starts listening when it is created, not when it is awaited. So create it before
the action that sends the request, then await it after; otherwise a fast response can arrive
before anything is listening. `wait_for_request` works the same way for requests.

A `Response` gives `status()`, `ok()`, `headers()`, `body()`, `text()`, `json()` and the
`request()` that led to it. A `Request` gives `url()`, `method()`, `headers()`,
`post_data()`, `post_data_json()`, `resource_type()`, `frame()`, `response()` and, for a
request that failed, `failure()`.

## Watching traffic

Handlers see every request and response as it happens:

```rust,ignore
{{#include ../../../samples/agito-samples/tests/book.rs:network_observe}}
```

`on_request`, `on_response`, `on_request_finished` and `on_request_failed` each take a
closure. `wait_for_network_idle(idle, timeout)` waits until no request has been in flight for
`idle`.

## Recording a HAR

```rust,ignore
page.start_har();
// ... exercise the page ...
page.stop_har_to("session.har")?;
```

`stop_har` returns the HAR as a `serde_json::Value` instead of writing a file. Browser
developer tools and many HTTP tools open HAR files.

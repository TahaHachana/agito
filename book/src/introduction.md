# Introduction

agito is a browser automation and testing framework for Rust. It drives Chromium-based
browsers (Chrome, Edge, Chromium and Chrome for Testing) over the Chrome DevTools Protocol,
talking to the browser directly: there is no WebDriver binary, Node.js process or other
sidecar between your code and the browser.

A test looks like this:

```rust,ignore
{{#include ../../samples/agito-samples/tests/book.rs:first_test}}
```

It runs under plain `cargo test`. The `page` it receives is a fresh page in its own browser
context, so tests never see each other's cookies, storage or routes, and every action waits
until the element can actually be used.

## What you get

- **Tests that run under `cargo test`.** `#[agito::test]` gives each test a fresh context and
  page in a browser shared by the test binary, and re-runs a test whose browser crashed.
- **Locators that wait.** Find elements by role, label, text, test id, CSS or XPath. Actions
  wait for the element to be attached, visible, enabled, stable and not covered, and
  assertions retry until they pass or time out.
- **The whole browser.** Network mocking and observation, HAR recording, iframes (including
  cross-origin ones), popups, uploads, downloads, dialogs, screenshots and PDFs.
- **A command-line tool.** `agito install` downloads a browser, `agito run` runs a suite with
  sharding, retries, quarantine and JUnit or HTML reports, and `agito screenshot` and
  `agito pdf` capture a page.
- **A plugin API.** Selector strategies and lifecycle hooks are plugins, and agito's own
  selectors are registered through the same API yours would use.

## How to read this book

[Getting started](getting-started/installation.md) gets a first test running. [How agito
is built](concepts/architecture.md) explains the layers and the objects you work with; it is
worth reading once, since the rest of the book leans on it. [Writing
tests](writing/locators.md) and [Running tests](running/agito-run.md) cover day-to-day use,
and the [reference](reference/cli.md) lists every command, setting and error.

The API reference for every type and method is on [docs.rs](https://docs.rs/agito).

## About the examples

The examples in this book are taken from a test file in agito's repository,
[`samples/agito-samples/tests/book.rs`](https://github.com/TahaHachana/agito/blob/main/samples/agito-samples/tests/book.rs),
which CI compiles and runs. They drive a small demo shop at `https://shop.test`, which the
test file serves through agito's own [network routing](writing/network.md), so nothing goes
to the network.

## Status

agito is young, and its version is below 1.0: expect the API to change between minor
versions. It is a Rust port of [Motus](https://github.com/DataficationSDK/Motus), a .NET
framework; [Coming from Motus](motus.md) covers the differences. Firefox, which Motus drives
over WebDriver BiDi, is not supported yet.

//! # agito
//!
//! Browser automation and testing for Rust. agito drives Chromium-based browsers (Chrome,
//! Edge, Chromium, Chrome for Testing) over the Chrome DevTools Protocol, directly: there is no
//! WebDriver binary or Node.js process in between.
//!
//! **New to agito? Start with [the agito book](https://tahahachana.github.io/agito/)**, which
//! explains how agito is built and walks through writing and running tests. This page is the
//! API reference.
//!
//! ```no_run
//! use agito::{expect, Page};
//!
//! #[agito::test]
//! async fn search_works(page: Page) -> agito::Result<()> {
//!     page.goto("https://example.com").await?;
//!     page.get_by_label("Search").fill("agito").await?;
//!     page.get_by_role("button", Some("Search")).click().await?;
//!     expect(&page).to_have_url("**/search**").await?;
//!     Ok(())
//! }
//! ```
//!
//! [`#[agito::test]`](macro@test) runs under plain `cargo test`: each test gets a fresh
//! [`BrowserContext`] and [`Page`] in a browser shared by its test binary.
//!
//! ## Where to start
//!
//! | To | Start at |
//! |---|---|
//! | Write a test | [`macro@test`], then [`Page`], [`Locator`] and [`expect()`] |
//! | Find elements | [`Page::get_by_role`], [`Page::locator`] and the other `get_by_*` methods |
//! | Mock or watch the network | [`Page::route`], [`Page::wait_for_response`], [`network`] |
//! | Work in iframes and popups | [`Page::frame_locator`], [`Page::wait_for_popup`] |
//! | Script a browser outside tests | [`launch()`], [`connect()`], [`Browser`] |
//! | Configure without code | [`config`]: `agito.config.json` and `AGITO_*` variables |
//! | Extend agito | [`mod@plugin`], [`selectors`] and [`#[agito::plugin]`](macro@plugin) |
//!
//! ## How the types fit together
//!
//! A [`Browser`] holds [`BrowserContext`]s, isolated profiles with their own cookies, storage
//! and routes. A context holds [`Page`]s, and a page holds [`Frame`]s. A [`Locator`] describes
//! how to find elements in a frame and finds them afresh every time it is used; actions on it
//! wait until the element can be used. All of these are cheap handles: cloning one clones a
//! pointer. Failures are an [`Error`], whose [`ErrorKind`] says what went wrong.
//!
//! The [book's architecture chapter](https://tahahachana.github.io/agito/concepts/architecture.html)
//! covers the layers underneath: [`transport`] sessions over one connection, typed by the
//! generated [`protocol`] crate.
//!
//! ## Features
//!
//! - `testing`: [`testing::FakeSocket`], a scripted browser connection for testing code built
//!   on agito without a browser.
//! - `integration`: enables agito's own browser tests. You do not need it.
//!
//! agito is a Rust port of the Motus .NET framework.

#![warn(missing_docs)]

pub mod accessibility;
pub mod browser;
pub mod config;
pub mod context;
pub mod error;
pub mod expect;
pub mod files;
pub mod frame;
pub mod handles;
pub mod harness;
pub mod input;
pub mod launch;
pub mod locator;
pub mod network;
pub mod page;
pub mod performance;
pub mod plugin;
pub mod selectors;
pub mod storage;
pub mod transport;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use agito_macros::{plugin, test};
pub use agito_protocol as protocol;
pub use browser::Browser;
pub use context::{BrowserContext, ColorScheme, ContextOptions, Cookie, Viewport};
pub use error::{AssertionFailure, Error, ErrorKind, Result};
pub use expect::{TextMatch, expect};
pub use files::{Download, FileChooser, FilePayload, InputFiles};
pub use frame::{Frame, FrameLocator};
pub use handles::{BoundingBox, ElementHandle};
pub use input::{ClickOptions, Keyboard, Mouse, MouseButton};
pub use launch::{BrowserChannel, ConnectOptions, LaunchOptions, connect, launch};
pub use locator::{ElementState, Locator};
pub use network::{ContinueOptions, FulfillOptions, Request, Route, RouteId};
pub use page::{
    ConsoleMessage, Dialog, ImageFormat, NavigateOptions, Page, PdfOptions, Response,
    ScreenshotOptions, UrlMatcher, WaitUntil,
};

#[doc(hidden)]
pub mod __private {
    pub use inventory;
}

//! # agito
//!
//! Browser automation and testing for Rust. agito talks to Chromium over the Chrome DevTools
//! Protocol directly, with no driver process in between, and is built on a plugin API its
//! own features use.
//!
//! ```no_run
//! use agito::{expect, Page};
//!
//! #[agito::test]
//! async fn search_works(page: Page) -> agito::Result<()> {
//!     page.goto("https://example.com").await?;
//!     page.locator("input[name=q]").fill("agito").await?;
//!     page.get_by_role("button", Some("Search")).click().await?;
//!     expect(&page).to_have_url("**/search**").await?;
//!     Ok(())
//! }
//! ```
//!
//! agito is a port of the Motus .NET framework.

pub mod browser;
pub mod config;
pub mod context;
pub mod error;
pub mod expect;
pub mod handles;
pub mod harness;
pub mod input;
pub mod launch;
pub mod locator;
pub mod page;
pub mod plugin;
pub mod selectors;
pub mod transport;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use agito_macros::{plugin, test};
pub use agito_protocol as protocol;
pub use browser::Browser;
pub use context::{BrowserContext, ColorScheme, ContextOptions, Cookie, Viewport};
pub use error::{AssertionFailure, Error, ErrorKind, Result};
pub use expect::{TextMatch, expect};
pub use handles::{BoundingBox, ElementHandle};
pub use input::{ClickOptions, Keyboard, Mouse, MouseButton};
pub use launch::{BrowserChannel, ConnectOptions, LaunchOptions, connect, launch};
pub use locator::{ElementState, Locator};
pub use page::{
    ConsoleMessage, Dialog, ImageFormat, NavigateOptions, Page, PdfOptions, Response,
    ScreenshotOptions, UrlMatcher, WaitUntil,
};

#[doc(hidden)]
pub mod __private {
    pub use inventory;
}

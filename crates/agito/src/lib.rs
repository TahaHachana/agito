//! # agito
//!
//! Browser automation and testing for Rust. agito talks to Chromium over the Chrome DevTools
//! Protocol and to Firefox over WebDriver BiDi, with no driver process in between.
//!
//! agito is a port of the Motus .NET framework.

pub mod error;
pub mod transport;

#[cfg(any(test, feature = "testing"))]
pub mod testing;

pub use agito_protocol as protocol;
pub use error::{AssertionFailure, Error, ErrorKind, Result};

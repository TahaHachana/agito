//! Protocol types for agito.
//!
//! [`cdp`] holds bindings for every Chrome DevTools Protocol domain, generated from the
//! vendored `protocol/*.json` files by `cargo xtask codegen`.

use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod cdp;

/// A CDP command: the parameters type, tied to its method name and response type.
pub trait Command: Serialize {
    /// The wire method name, such as `Page.navigate`.
    const METHOD: &'static str;
    /// What the browser answers with.
    type Response: DeserializeOwned;
}

/// A CDP event payload, tied to its wire name.
pub trait Event: DeserializeOwned {
    /// The wire event name, such as `Page.frameNavigated`.
    const NAME: &'static str;
}

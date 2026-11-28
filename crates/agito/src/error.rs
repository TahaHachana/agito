//! The error type shared by every agito operation.
//!
//! One enum replaces Motus's exception hierarchy. The variants keep the same fields, so a
//! failure report can say which selector, URL, check or timeout was involved.

use std::sync::Arc;
use std::time::Duration;

/// A `Result` whose error is [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong while driving a browser.
///
/// The details live in [`ErrorKind`]; the error itself is one pointer wide so `Result`s stay
/// small on the happy path.
#[derive(Clone)]
pub struct Error(Box<ErrorKind>);

/// Prints the message, so `fn main() -> agito::Result<()>` reports failures readably.
/// Match on [`Error::kind`] for the structured details.
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            return f.debug_tuple("Error").field(&self.0).finish();
        }
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl Error {
    /// What went wrong, with its details.
    pub fn kind(&self) -> &ErrorKind {
        &self.0
    }

    /// Takes the details out.
    pub fn into_kind(self) -> ErrorKind {
        *self.0
    }

    pub(crate) fn other(message: impl Into<String>) -> Self {
        ErrorKind::Other(message.into()).into()
    }

    pub(crate) fn invalid_argument(message: impl Into<String>) -> Self {
        ErrorKind::InvalidArgument(message.into()).into()
    }

    pub(crate) fn not_supported(message: impl Into<String>) -> Self {
        ErrorKind::NotSupported(message.into()).into()
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&*self.0)
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Self {
        Error(Box::new(kind))
    }
}

/// The kinds of failure, mirroring Motus's exception hierarchy.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A navigation did not reach the requested load state in time.
    #[error("{message}")]
    NavigationTimeout {
        /// The URL being loaded.
        url: String,
        /// How long it waited.
        timeout: Duration,
        /// The last network events seen, to explain the wait.
        last_network_events: Vec<String>,
        /// The full message.
        message: String,
    },

    /// A wait condition did not become true in time.
    #[error("{message}")]
    WaitTimeout {
        /// What was being waited for.
        condition: String,
        /// How long it waited.
        timeout: Duration,
        /// The last value seen.
        last_evaluated_value: Option<String>,
        /// The full message.
        message: String,
    },

    /// An action's actionability checks did not pass in time.
    #[error("{message}")]
    ActionTimeout {
        /// The locator's selector.
        selector: String,
        /// The check that did not pass: `visible`, `enabled`, `editable`, `scrollIntoView`,
        /// `stable`, `receivesEvents`, or `resolve` for an element never found.
        failed_check: String,
        /// What was seen of the element.
        element_state: Option<String>,
        /// The page's URL.
        page_url: String,
        /// How long it waited.
        timeout: Duration,
        /// The full message.
        message: String,
    },

    /// The browser did not answer a protocol command in time.
    #[error("'{method}' got no answer within {timeout:?}")]
    CommandTimeout {
        /// The command that got no answer.
        method: String,
        /// How long it waited.
        timeout: Duration,
    },

    /// A selector matched nothing.
    #[error("No element found for selector: {selector}")]
    ElementNotFound {
        /// The selector.
        selector: String,
        /// The page's URL.
        page_url: String,
        /// Where the search stopped matching, when known.
        dom_snapshot: Option<String>,
    },

    /// Never raised: an action acts on the first element its locator matches. Kept so code that
    /// matches on it still compiles.
    #[deprecated(
        since = "0.3.1",
        note = "never raised, since an action uses the first match; it will be removed in 0.4"
    )]
    #[error("Selector '{selector}' matched {matched} elements, expected one.")]
    AmbiguousSelector {
        /// The selector.
        selector: String,
        /// The page's URL.
        page_url: String,
        /// How many elements matched.
        matched: usize,
    },

    /// A navigation failed outright, such as `net::ERR_NAME_NOT_RESOLVED`.
    #[error("{message}")]
    Navigation {
        /// The URL being loaded.
        url: String,
        /// The browser's error, such as `net::ERR_NAME_NOT_RESOLVED`.
        error_code: String,
        /// The page's URL.
        page_url: String,
        /// The full message.
        message: String,
    },

    /// The page, context, session or browser went away underneath the call.
    #[error("{message}")]
    TargetClosed {
        /// What went away: `page`, `context`, `session` or `browser`.
        target_type: String,
        /// Its id, when known.
        target_id: Option<String>,
        /// The full message.
        message: String,
    },

    /// An `expect` assertion failed.
    #[error("{}", .0.message)]
    Assertion(AssertionFailure),

    /// The browser answered a command with an error.
    #[error("{message}")]
    Protocol {
        /// The protocol error code.
        code: Option<i64>,
        /// The command that failed.
        method: Option<String>,
        /// The browser's message.
        message: String,
    },

    /// A script run in the page threw, or returned something that could not be decoded.
    #[error("{message}")]
    Evaluation {
        /// What the script threw, or why its result could not be decoded.
        message: String,
    },

    /// The feature needs something the current browser transport cannot do.
    #[error("{0}")]
    NotSupported(String),

    /// The caller passed something that cannot work.
    #[error("{0}")]
    InvalidArgument(String),

    /// Launching or talking to the browser process failed at the OS level.
    #[error("{context}: {source}")]
    Io {
        /// What agito was doing.
        context: String,
        /// The underlying error.
        source: Arc<std::io::Error>,
    },

    /// Anything else, with a message.
    #[error("{0}")]
    Other(String),
}

/// The details of a failed assertion.
#[derive(Debug, Clone, PartialEq)]
pub struct AssertionFailure {
    /// What was expected.
    pub expected: Option<String>,
    /// What was last seen.
    pub actual: Option<String>,
    /// The locator's selector, for assertions on a locator.
    pub selector: Option<String>,
    /// The page's URL.
    pub page_url: Option<String>,
    /// How long the assertion retried.
    pub timeout: Duration,
    /// The full message.
    pub message: String,
}

impl Error {
    /// Whether this failure means the browser (or the target being driven) is gone, the one
    /// kind of failure a test harness may retry. Mirrors Motus's `BrowserFailure.IsBrowserLost`.
    pub fn is_browser_lost(&self) -> bool {
        matches!(self.kind(), ErrorKind::TargetClosed { .. })
    }

    /// Whether this is any kind of timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(
            self.kind(),
            ErrorKind::NavigationTimeout { .. }
                | ErrorKind::WaitTimeout { .. }
                | ErrorKind::ActionTimeout { .. }
                | ErrorKind::CommandTimeout { .. }
        )
    }

    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        ErrorKind::Io {
            context: context.into(),
            source: Arc::new(source),
        }
        .into()
    }

    pub(crate) fn target_closed(
        target_type: &str,
        target_id: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        ErrorKind::TargetClosed {
            target_type: target_type.to_owned(),
            target_id: target_id.map(str::to_owned),
            message: message.into(),
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_lost_only_for_target_closed() {
        assert!(Error::target_closed("session", None, "gone").is_browser_lost());
        assert!(!Error::other("x").is_browser_lost());
        assert!(
            !Error::from(ErrorKind::Protocol {
                code: Some(-32000),
                method: None,
                message: "x".into()
            })
            .is_browser_lost()
        );
    }

    #[test]
    fn messages_match_motus() {
        let e = ErrorKind::ElementNotFound {
            selector: "#a".into(),
            page_url: "about:blank".into(),
            dom_snapshot: None,
        };
        assert_eq!(e.to_string(), "No element found for selector: #a");
    }
}

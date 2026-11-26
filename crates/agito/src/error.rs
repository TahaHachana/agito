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
#[derive(Debug, Clone)]
pub struct Error(Box<ErrorKind>);

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
        url: String,
        timeout: Duration,
        last_network_events: Vec<String>,
        message: String,
    },

    /// A wait condition did not become true in time.
    #[error("{message}")]
    WaitTimeout {
        condition: String,
        timeout: Duration,
        last_evaluated_value: Option<String>,
        message: String,
    },

    /// An action's actionability checks did not pass in time.
    #[error("{message}")]
    ActionTimeout {
        selector: String,
        failed_check: String,
        element_state: Option<String>,
        page_url: String,
        timeout: Duration,
        message: String,
    },

    /// The browser did not answer a protocol command in time.
    #[error("'{method}' got no answer within {timeout:?}")]
    CommandTimeout { method: String, timeout: Duration },

    /// A selector matched nothing.
    #[error("No element found for selector: {selector}")]
    ElementNotFound {
        selector: String,
        page_url: String,
        dom_snapshot: Option<String>,
    },

    /// A selector that must match one element matched several.
    #[error("Selector '{selector}' matched {matched} elements, expected one.")]
    AmbiguousSelector {
        selector: String,
        page_url: String,
        matched: usize,
    },

    /// A navigation failed outright, such as `net::ERR_NAME_NOT_RESOLVED`.
    #[error("{message}")]
    Navigation {
        url: String,
        error_code: String,
        page_url: String,
        message: String,
    },

    /// The page, context, session or browser went away underneath the call.
    #[error("{message}")]
    TargetClosed {
        target_type: String,
        target_id: Option<String>,
        message: String,
    },

    /// An `expect` assertion failed.
    #[error("{}", .0.message)]
    Assertion(AssertionFailure),

    /// The browser answered a command with an error.
    #[error("{message}")]
    Protocol {
        code: Option<i64>,
        method: Option<String>,
        message: String,
    },

    /// A script run in the page threw, or returned something that could not be decoded.
    #[error("{message}")]
    Evaluation { message: String },

    /// The feature needs something the current browser transport cannot do.
    #[error("{0}")]
    NotSupported(String),

    /// The caller passed something that cannot work.
    #[error("{0}")]
    InvalidArgument(String),

    /// Launching or talking to the browser process failed at the OS level.
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: Arc<std::io::Error>,
    },

    /// Anything else, with a message.
    #[error("{0}")]
    Other(String),
}

/// The details of a failed assertion.
#[derive(Debug, Clone, PartialEq)]
pub struct AssertionFailure {
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub selector: Option<String>,
    pub page_url: Option<String>,
    pub timeout: Duration,
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
        let e = ErrorKind::AmbiguousSelector {
            selector: "li".into(),
            page_url: String::new(),
            matched: 3,
        };
        assert_eq!(
            e.to_string(),
            "Selector 'li' matched 3 elements, expected one."
        );
    }
}

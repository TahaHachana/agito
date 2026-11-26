//! Web-first assertions: each one re-checks until it passes or its timeout runs out.
//!
//! ```no_run
//! # async fn demo(page: agito::Page) -> agito::Result<()> {
//! use agito::expect;
//! expect(&page.locator("h1")).to_have_text("Welcome").await?;
//! expect(&page.locator(".spinner")).not().to_be_visible().await?;
//! expect(&page).to_have_url("**/dashboard").await?;
//! # Ok(()) }
//! ```

// `.not()` reads as the assertion it builds (`expect(x).not().to_be_visible()`), as `.Not` did
// in Motus; it is not the `!` operator.
#![allow(clippy::should_implement_trait)]

use std::future::Future;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use crate::config;
use crate::error::{AssertionFailure, Error, ErrorKind, Result};
use crate::locator::{Locator, is_retryable};
use crate::page::{Page, Response, UrlMatcher};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const NOT_EVALUATED: &str = "<not evaluated>";
const NOT_FOUND: &str = "<element not found>";

/// The default assertion timeout: `assertions.timeout` from config, else 10 seconds.
pub fn default_timeout() -> Duration {
    Duration::from_millis(config::get().assertions.timeout.unwrap_or(10_000))
}

/// Something `expect` can assert on.
pub trait Expect {
    type Assertions;
    fn expect(&self) -> Self::Assertions;
}

/// Starts an assertion on a locator, a page or a response.
pub fn expect<T: Expect + ?Sized>(subject: &T) -> T::Assertions {
    subject.expect()
}

/// Text to compare against: an exact string or a regex.
#[derive(Debug, Clone)]
pub enum TextMatch {
    Exact(String),
    Regex(Regex),
}

impl TextMatch {
    fn matches(&self, actual: &str) -> bool {
        match self {
            TextMatch::Exact(s) => s == actual,
            TextMatch::Regex(r) => r.is_match(actual),
        }
    }

    fn describe(&self) -> String {
        match self {
            TextMatch::Exact(s) => s.clone(),
            TextMatch::Regex(r) => format!("/{r}/"),
        }
    }
}

impl From<&str> for TextMatch {
    fn from(s: &str) -> Self {
        TextMatch::Exact(s.to_owned())
    }
}

impl From<String> for TextMatch {
    fn from(s: String) -> Self {
        TextMatch::Exact(s)
    }
}

impl From<Regex> for TextMatch {
    fn from(r: Regex) -> Self {
        TextMatch::Regex(r)
    }
}

impl From<&Regex> for TextMatch {
    fn from(r: &Regex) -> Self {
        TextMatch::Regex(r.clone())
    }
}

/// Options shared by every assertion builder.
#[derive(Debug, Clone, Default)]
struct Settings {
    negate: bool,
    timeout: Option<Duration>,
    message: Option<String>,
}

/// One probe of the condition: whether it held, and what was seen.
type Probe = Result<(bool, String)>;

/// Polls `condition` every 100ms until it holds (or, negated, stops holding).
async fn retry_until<F, Fut>(
    settings: &Settings,
    name: &str,
    expected: &str,
    selector: Option<String>,
    page_url: impl Fn() -> Option<String>,
    mut condition: F,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Probe>,
{
    let timeout = settings.timeout.unwrap_or_else(default_timeout);
    let deadline = Instant::now() + timeout;
    let mut last_actual = NOT_EVALUATED.to_owned();
    let mut last_error: Option<Error> = None;
    loop {
        match condition().await {
            Ok((passed, actual)) => {
                last_actual = actual;
                if passed != settings.negate {
                    return Ok(());
                }
            }
            Err(e) if is_retryable(&e) || e.is_browser_lost() => last_error = Some(e),
            Err(e) => return Err(e),
        }
        if Instant::now() + POLL_INTERVAL > deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    // The browser went away before anything was evaluated: say that, so a harness can retry.
    if last_actual == NOT_EVALUATED
        && let Some(e) = last_error.filter(|e| e.is_browser_lost())
    {
        return Err(e);
    }
    let not = if settings.negate { "NOT " } else { "" };
    let page_url = page_url();
    let message = settings.message.clone().unwrap_or_else(|| {
        let mut m = format!(
            "Assertion {not}{name} failed after {}ms. Expected: {not}{expected}. Received: {last_actual}.",
            timeout.as_millis()
        );
        if let Some(s) = &selector {
            m.push_str(&format!(" Selector: {s}."));
        }
        if let Some(u) = &page_url {
            m.push_str(&format!(" Page: {u}."));
        }
        m
    });
    Err(ErrorKind::Assertion(AssertionFailure {
        expected: Some(format!("{not}{expected}")),
        actual: Some(last_actual),
        selector,
        page_url,
        timeout,
        message,
    })
    .into())
}

/// Assertions on a [`Locator`].
#[must_use = "assertions do nothing until awaited"]
pub struct LocatorAssertions {
    locator: Locator,
    settings: Settings,
}

impl Expect for Locator {
    type Assertions = LocatorAssertions;
    fn expect(&self) -> LocatorAssertions {
        LocatorAssertions {
            locator: self.clone(),
            settings: Settings::default(),
        }
    }
}

impl LocatorAssertions {
    /// Inverts the assertion.
    pub fn not(mut self) -> Self {
        self.settings.negate = !self.settings.negate;
        self
    }

    /// Overrides the timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.settings.timeout = Some(timeout);
        self
    }

    /// Replaces the failure message.
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.settings.message = Some(message.into());
        self
    }

    /// Runs a probe. A missing element counts as `when_absent` (by default: failing the
    /// assertion, whichever way round it is).
    async fn run<F, Fut>(
        &self,
        name: &str,
        expected: &str,
        when_absent: Option<bool>,
        probe: F,
    ) -> Result<()>
    where
        F: Fn(Locator) -> Fut,
        Fut: Future<Output = Probe>,
    {
        let absent = when_absent.unwrap_or(self.settings.negate);
        let page = self.locator.page().clone();
        retry_until(
            &self.settings,
            name,
            expected,
            Some(self.locator.description()),
            || Some(page.url()),
            || {
                let fut = probe(self.locator.clone());
                async move {
                    match fut.await {
                        Err(e) if matches!(e.kind(), ErrorKind::ElementNotFound { .. }) => {
                            Ok((absent, NOT_FOUND.to_owned()))
                        }
                        other => other,
                    }
                }
            },
        )
        .await
    }

    async fn bool_probe(
        &self,
        name: &str,
        expected: &str,
        js: &'static str,
        when_absent: Option<bool>,
    ) -> Result<()> {
        self.run(name, expected, when_absent, |l| async move {
            let v: bool = l.probe_eval(js, &[]).await?;
            Ok((v, v.to_string()))
        })
        .await
    }

    pub async fn to_be_visible(self) -> Result<()> {
        self.bool_probe("to_be_visible", "visible", Locator::VISIBLE_JS, Some(false))
            .await
    }

    pub async fn to_be_hidden(self) -> Result<()> {
        self.run("to_be_hidden", "hidden", Some(true), |l| async move {
            let visible: bool = l.probe_eval(Locator::VISIBLE_JS, &[]).await?;
            Ok((!visible, (!visible).to_string()))
        })
        .await
    }

    pub async fn to_be_enabled(self) -> Result<()> {
        self.bool_probe("to_be_enabled", "enabled", Locator::ENABLED_JS, None)
            .await
    }

    pub async fn to_be_disabled(self) -> Result<()> {
        self.run("to_be_disabled", "disabled", None, |l| async move {
            let enabled: bool = l.probe_eval(Locator::ENABLED_JS, &[]).await?;
            Ok((!enabled, (!enabled).to_string()))
        })
        .await
    }

    pub async fn to_be_editable(self) -> Result<()> {
        self.bool_probe("to_be_editable", "editable", Locator::EDITABLE_JS, None)
            .await
    }

    pub async fn to_be_checked(self) -> Result<()> {
        self.run("to_be_checked", "checked", None, |l| async move {
            let v = l.probe_checked().await?;
            Ok((v, v.to_string()))
        })
        .await
    }

    pub async fn to_be_empty(self) -> Result<()> {
        self.bool_probe(
            "to_be_empty",
            "empty",
            "function() { var t = this.tagName.toLowerCase(); \
             if (t === 'input' || t === 'textarea' || t === 'select') return this.value === ''; \
             return this.textContent.trim() === ''; }",
            None,
        )
        .await
    }

    pub async fn to_be_focused(self) -> Result<()> {
        self.bool_probe(
            "to_be_focused",
            "focused",
            "function() { return document.activeElement === this; }",
            None,
        )
        .await
    }

    pub async fn to_be_attached(self) -> Result<()> {
        self.run("to_be_attached", "attached", Some(false), |l| async move {
            let count = l.count().await?;
            Ok((count > 0, format!("count={count}")))
        })
        .await
    }

    pub async fn to_be_detached(self) -> Result<()> {
        self.run("to_be_detached", "detached", Some(true), |l| async move {
            let count = l.count().await?;
            Ok((count == 0, format!("count={count}")))
        })
        .await
    }

    /// The element's whole `textContent` matches.
    pub async fn to_have_text(self, expected: impl Into<TextMatch>) -> Result<()> {
        let expected = expected.into();
        let description = expected.describe();
        self.run("to_have_text", &description, None, |l| {
            let expected = expected.clone();
            async move {
                let text = l.probe_text().await?;
                Ok((expected.matches(&text), text))
            }
        })
        .await
    }

    /// The element's `textContent` contains `expected`.
    pub async fn to_contain_text(self, expected: &str) -> Result<()> {
        self.run("to_contain_text", expected, None, |l| async move {
            let text = l.probe_text().await?;
            Ok((text.contains(expected), text))
        })
        .await
    }

    /// An input's value matches.
    pub async fn to_have_value(self, expected: impl Into<TextMatch>) -> Result<()> {
        let expected = expected.into();
        let description = expected.describe();
        self.run("to_have_value", &description, None, |l| {
            let expected = expected.clone();
            async move {
                let value: Option<String> = l
                    .probe_eval("function() { return this.value; }", &[])
                    .await?;
                let value = value.unwrap_or_default();
                Ok((expected.matches(&value), value))
            }
        })
        .await
    }

    /// An attribute's value matches.
    pub async fn to_have_attribute(self, name: &str, expected: impl Into<TextMatch>) -> Result<()> {
        let expected = expected.into();
        let description = format!("{name}={}", expected.describe());
        self.run("to_have_attribute", &description, None, |l| {
            let expected = expected.clone();
            async move {
                let attr: Option<String> = l
                    .probe_eval(
                        "function(n) { return this.getAttribute(n); }",
                        &[Value::from(name)],
                    )
                    .await?;
                Ok((
                    attr.as_deref().is_some_and(|a| expected.matches(a)),
                    attr.unwrap_or_else(|| "<null>".into()),
                ))
            }
        })
        .await
    }

    /// The element's class list contains `class_name`.
    pub async fn to_have_class(self, class_name: &str) -> Result<()> {
        self.run("to_have_class", class_name, None, |l| async move {
            let classes: String = l
                .probe_eval("function() { return this.className.toString(); }", &[])
                .await?;
            Ok((classes.split_whitespace().any(|c| c == class_name), classes))
        })
        .await
    }

    /// A computed style property equals `expected`.
    pub async fn to_have_css(self, property: &str, expected: &str) -> Result<()> {
        let description = format!("{property}: {expected}");
        self.run("to_have_css", &description, None, |l| async move {
            let value: String = l
                .probe_eval(
                    "function(p) { return window.getComputedStyle(this).getPropertyValue(p); }",
                    &[Value::from(property)],
                )
                .await?;
            Ok((value == expected, value))
        })
        .await
    }

    /// Exactly `expected` elements match.
    pub async fn to_have_count(self, expected: usize) -> Result<()> {
        self.run(
            "to_have_count",
            &expected.to_string(),
            Some(expected == 0),
            |l| async move {
                let count = l.count().await?;
                Ok((count == expected, count.to_string()))
            },
        )
        .await
    }
}

/// Assertions on a [`Page`].
#[must_use = "assertions do nothing until awaited"]
pub struct PageAssertions {
    page: Page,
    settings: Settings,
}

impl Expect for Page {
    type Assertions = PageAssertions;
    fn expect(&self) -> PageAssertions {
        PageAssertions {
            page: self.clone(),
            settings: Settings::default(),
        }
    }
}

impl PageAssertions {
    pub fn not(mut self) -> Self {
        self.settings.negate = !self.settings.negate;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.settings.timeout = Some(timeout);
        self
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.settings.message = Some(message.into());
        self
    }

    /// The document title matches.
    pub async fn to_have_title(self, expected: impl Into<TextMatch>) -> Result<()> {
        let expected = expected.into();
        let page = self.page.clone();
        retry_until(
            &self.settings,
            "to_have_title",
            &expected.describe(),
            None,
            || Some(page.url()),
            || {
                let page = page.clone();
                let expected = expected.clone();
                async move {
                    let title = page.title().await?;
                    Ok((expected.matches(&title), title))
                }
            },
        )
        .await
    }

    /// The URL matches an exact URL, a glob (`**/search**`), or a regex.
    pub async fn to_have_url(self, expected: impl Into<UrlMatcher>) -> Result<()> {
        let matcher = expected.into();
        let page = self.page.clone();
        retry_until(
            &self.settings,
            "to_have_url",
            &matcher.describe(),
            None,
            || None,
            || {
                let url = page.url();
                let passed = matcher.matches(&url);
                async move { Ok((passed, url)) }
            },
        )
        .await
    }
}

/// Assertions on a [`Response`]. These check once; a response does not change.
pub struct ResponseAssertions {
    response: Response,
    negate: bool,
}

impl Expect for Response {
    type Assertions = ResponseAssertions;
    fn expect(&self) -> ResponseAssertions {
        ResponseAssertions {
            response: self.clone(),
            negate: false,
        }
    }
}

impl ResponseAssertions {
    pub fn not(mut self) -> Self {
        self.negate = !self.negate;
        self
    }

    fn check(&self, name: &str, passed: bool, expected: &str) -> Result<()> {
        if passed != self.negate {
            return Ok(());
        }
        let not = if self.negate { "NOT " } else { "" };
        Err(ErrorKind::Assertion(AssertionFailure {
            expected: Some(format!("{not}{expected}")),
            actual: Some(self.response.status.to_string()),
            selector: None,
            page_url: Some(self.response.url.clone()),
            timeout: Duration::ZERO,
            message: format!(
                "Assertion {not}{name} failed. Expected: {not}{expected}. Received: {} {}. URL: {}.",
                self.response.status, self.response.status_text, self.response.url
            ),
        })
        .into())
    }

    /// The status is 2xx.
    pub fn to_be_ok(&self) -> Result<()> {
        self.check("to_be_ok", self.response.ok(), "status 200-299")
    }

    /// The status equals `status`.
    pub fn to_have_status(&self, status: u16) -> Result<()> {
        self.check(
            "to_have_status",
            self.response.status == status,
            &format!("status {status}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn passes_once_condition_holds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let settings = Settings {
            timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        retry_until(
            &settings,
            "x",
            "y",
            None,
            || None,
            || {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                async move { Ok((n >= 2, n.to_string())) }
            },
        )
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn failure_reports_last_actual() {
        let settings = Settings {
            timeout: Some(Duration::from_millis(250)),
            ..Default::default()
        };
        let err = retry_until(
            &settings,
            "to_have_text",
            "a",
            Some("#x".into()),
            || Some("u".into()),
            || async { Ok((false, "b".to_owned())) },
        )
        .await
        .unwrap_err();
        let ErrorKind::Assertion(f) = err.kind() else {
            panic!("{err:?}")
        };
        assert_eq!(f.actual.as_deref(), Some("b"));
        assert_eq!(f.expected.as_deref(), Some("a"));
        assert!(f.message.contains("Assertion to_have_text failed after 250ms. Expected: a. Received: b. Selector: #x. Page: u."));
    }

    #[tokio::test]
    async fn negation_flips_and_labels() {
        let settings = Settings {
            negate: true,
            timeout: Some(Duration::from_millis(150)),
            ..Default::default()
        };
        retry_until(
            &settings,
            "n",
            "e",
            None,
            || None,
            || async { Ok((false, String::new())) },
        )
        .await
        .unwrap();
        let err = retry_until(
            &settings,
            "n",
            "e",
            None,
            || None,
            || async { Ok((true, "z".into())) },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().starts_with("Assertion NOT n failed"));
    }

    #[tokio::test]
    async fn browser_loss_before_any_evaluation_is_rethrown() {
        let settings = Settings {
            timeout: Some(Duration::from_millis(150)),
            ..Default::default()
        };
        let err = retry_until(
            &settings,
            "n",
            "e",
            None,
            || None,
            || async { Err::<(bool, String), _>(Error::target_closed("session", None, "gone")) },
        )
        .await
        .unwrap_err();
        assert!(err.is_browser_lost());
    }

    #[tokio::test]
    async fn non_retryable_errors_surface_immediately() {
        let settings = Settings {
            timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        let started = Instant::now();
        let err = retry_until(
            &settings,
            "n",
            "e",
            None,
            || None,
            || async { Err::<(bool, String), _>(Error::invalid_argument("bad")) },
        )
        .await
        .unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::InvalidArgument(_)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn response_assertions() {
        let r = Response {
            url: "u".into(),
            status: 404,
            status_text: "Not Found".into(),
            headers: Default::default(),
            mime_type: String::new(),
        };
        assert!(expect(&r).to_be_ok().is_err());
        assert!(expect(&r).not().to_be_ok().is_ok());
        assert!(expect(&r).to_have_status(404).is_ok());
    }
}

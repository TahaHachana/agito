//! Runtime support for `#[agito::test]`. Not meant to be called directly.
//!
//! Every test in a binary shares one browser, launched on first use, and gets its own
//! context and page. Tests run on one process-wide tokio runtime, so the shared browser's
//! background tasks outlive any single test. A test that fails because the browser was lost
//! is relaunched and re-run (once by default); assertion failures are never re-run.

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use futures::FutureExt;
use tokio::sync::{Mutex, Semaphore};

use crate::accessibility::AccessibilityMode;
use crate::browser::Browser;
use crate::config;
use crate::context::{BrowserContext, ContextOptions};
use crate::error::{Error, Result};
use crate::launch::{LaunchOptions, launch};
use crate::page::{Page, ScreenshotOptions};

pub mod report;

pub use report::TestRecord;

static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("agito-test")
        .build()
        .expect("the agito test runtime starts")
});

/// The runtime tests run on.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    &RUNTIME
}

struct Shared {
    browser: Option<Browser>,
    last_failure: Option<(Instant, Error)>,
}

static SHARED: LazyLock<Mutex<Shared>> = LazyLock::new(|| {
    Mutex::new(Shared {
        browser: None,
        last_failure: None,
    })
});

static CONTEXT_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| {
    let slots = std::env::var("AGITO_MAX_CONTEXTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    Semaphore::new(slots)
});

const LAUNCH_ATTEMPTS: u32 = 3;
const FAILURE_MEMO: Duration = Duration::from_secs(30);

/// The browser shared by every test in this binary, launched (or relaunched) as needed.
pub async fn shared_browser() -> Result<Browser> {
    let mut shared = SHARED.lock().await;
    if let Some(browser) = &shared.browser
        && browser.is_connected()
    {
        return Ok(browser.clone());
    }
    if let Some((at, error)) = &shared.last_failure
        && at.elapsed() < FAILURE_MEMO
    {
        return Err(error.clone());
    }
    let mut last = None;
    for attempt in 1..=LAUNCH_ATTEMPTS {
        match launch(LaunchOptions::default()).await {
            Ok(browser) => {
                shared.browser = Some(browser.clone());
                shared.last_failure = None;
                return Ok(browser);
            }
            Err(e) => {
                eprintln!("agito: browser launch attempt {attempt} failed: {e}");
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
            }
        }
    }
    let error = last.expect("at least one attempt ran");
    shared.last_failure = Some((Instant::now(), error.clone()));
    Err(error)
}

/// What a test can ask for.
#[derive(Clone)]
pub struct Fixtures {
    /// The browser the test binary shares.
    pub browser: Browser,
    /// The test's own context.
    pub context: BrowserContext,
    /// A new page in that context.
    pub page: Page,
}

/// Per-test settings from the attribute.
#[derive(Clone)]
pub struct TestOptions {
    /// Re-runs after a lost browser.
    pub retries: u32,
    /// Makes the options for the test's context.
    pub context: fn() -> ContextOptions,
    /// Why the test is quarantined: it still runs, but a failure is reported, not raised.
    pub quarantine: Option<&'static str>,
    /// The performance budget `to_meet_performance_budget` holds the test to, in place of
    /// the configuration's.
    pub perf_budget: Option<crate::performance::PerformanceBudget>,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            retries: 1,
            context: ContextOptions::default,
            quarantine: None,
            perf_budget: None,
        }
    }
}

/// How a test body's return value reports failure.
pub trait TestOutcome {
    /// `Err` with a message when the test failed.
    fn outcome(&self) -> std::result::Result<(), String>;
    /// Whether the failure was the browser going away.
    fn browser_lost(&self) -> bool;
}

impl TestOutcome for () {
    fn outcome(&self) -> std::result::Result<(), String> {
        Ok(())
    }
    fn browser_lost(&self) -> bool {
        false
    }
}

impl<E: Debug + 'static> TestOutcome for std::result::Result<(), E> {
    fn outcome(&self) -> std::result::Result<(), String> {
        match self {
            Ok(()) => Ok(()),
            Err(e) => match (e as &dyn Any).downcast_ref::<Error>() {
                Some(agito) => Err(agito.to_string()),
                None => Err(format!("{e:?}")),
            },
        }
    }
    fn browser_lost(&self) -> bool {
        match self {
            Ok(()) => false,
            Err(e) => (e as &dyn Any)
                .downcast_ref::<Error>()
                .is_some_and(Error::is_browser_lost),
        }
    }
}

/// Runs one test: sets up fixtures, runs the body, tears down, and retries on browser loss.
/// Panics (failing the test) when the body fails, unless the test is quarantined.
pub fn run<F, Fut, R>(name: &str, options: TestOptions, body: F)
where
    F: Fn(Fixtures) -> Fut,
    Fut: Future<Output = R>,
    R: TestOutcome,
{
    let started = Instant::now();
    let attempts = options.retries + 1;
    let mut attempt = 0;
    let (result, artifacts) = loop {
        attempt += 1;
        let last_attempt = attempt == attempts;
        let (result, artifacts) = RUNTIME.block_on(run_attempt(name, &options, &body));
        let retry = match &result {
            Attempt::SetupFailed(e) => e
                .is_browser_lost()
                .then(|| format!("browser lost during setup ({e})")),
            Attempt::Failed {
                message,
                browser_lost: true,
            } => Some(format!("browser lost ({message})")),
            _ => None,
        };
        match retry {
            Some(why) if !last_attempt => eprintln!(
                "[RETRY] {name}: {why}; attempt {} of {attempts}",
                attempt + 1
            ),
            _ => break (result, artifacts),
        }
    };

    let (message, browser_lost) = match &result {
        Attempt::Passed => (None, false),
        Attempt::SetupFailed(e) => (
            Some(format!("{name}: could not set up the browser: {e}")),
            e.is_browser_lost(),
        ),
        Attempt::Failed {
            message,
            browser_lost,
        } => (Some(message.clone()), *browser_lost),
        Attempt::Panicked(panic) => (Some(panic_message(panic.as_ref())), false),
    };
    report::write(&TestRecord {
        name: name.to_owned(),
        passed: message.is_none(),
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
        attempts: attempt,
        message: message.clone(),
        browser_lost,
        quarantine: options.quarantine.map(str::to_owned),
        screenshot: artifacts.screenshot,
        trace: artifacts.trace,
        accessibility: artifacts.accessibility,
    });

    let Some(message) = message else {
        return;
    };
    if let Some(reason) = options.quarantine {
        eprintln!("[QUARANTINED] {name} failed, which does not fail the run ({reason}): {message}");
        return;
    }
    match result {
        Attempt::Panicked(panic) => std::panic::resume_unwind(panic),
        _ => panic!("{message}"),
    }
}

/// What an attempt left behind besides its outcome.
#[derive(Default)]
struct Artifacts {
    /// The failure screenshot, if one was saved.
    screenshot: Option<std::path::PathBuf>,
    /// The failure trace, if one was saved.
    trace: Option<std::path::PathBuf>,
    /// What automatic accessibility audits found.
    accessibility: Vec<crate::accessibility::Violation>,
}

/// One attempt: fixtures, body, teardown, and what it left behind.
async fn run_attempt<F, Fut, R>(name: &str, options: &TestOptions, body: &F) -> (Attempt, Artifacts)
where
    F: Fn(Fixtures) -> Fut,
    Fut: Future<Output = R>,
    R: TestOutcome,
{
    let _slot = CONTEXT_SLOTS
        .acquire()
        .await
        .expect("semaphore is never closed");
    let fixtures = match setup(options).await {
        Ok(f) => f,
        Err(e) => return (Attempt::SetupFailed(e), Artifacts::default()),
    };
    // A trace of every test, kept for the ones that fail.
    let tracing = config::get().failure.trace == Some(true)
        && fixtures
            .context
            .start_tracing(crate::TracingOptions {
                title: Some(name.to_owned()),
                ..Default::default()
            })
            .await
            .is_ok();
    let run = AssertUnwindSafe(body(fixtures.clone())).catch_unwind();
    let outcome = match options.perf_budget.clone() {
        Some(budget) => crate::performance::TEST_BUDGET.scope(budget, run).await,
        None => run.await,
    };
    let accessibility = fixtures.context.accessibility_violations();
    let a11y = &fixtures.context.options().accessibility;
    let a11y_failure = (a11y.mode == AccessibilityMode::Enforce)
        .then(|| a11y.failures(&accessibility))
        .filter(|failing| !failing.is_empty())
        .map(|failing| {
            let lines: Vec<String> = failing.iter().map(|v| format!("  {v}")).collect();
            format!(
                "{} accessibility violation(s), in enforce mode:\n{}",
                failing.len(),
                lines.join("\n")
            )
        });
    if a11y.mode == AccessibilityMode::Warn && !accessibility.is_empty() {
        eprintln!(
            "[ACCESSIBILITY] {name}: {} violation(s):",
            accessibility.len()
        );
        for v in &accessibility {
            eprintln!("  {v}");
        }
    }
    let failed = !matches!(&outcome, Ok(r) if r.outcome().is_ok()) || a11y_failure.is_some();
    let screenshot = if failed {
        capture_failure(name, &fixtures.page).await
    } else {
        None
    };
    let trace = match (tracing, failed) {
        (false, _) => None,
        (true, true) => save_failure_trace(name, &fixtures.context).await,
        (true, false) => {
            let _ = fixtures.context.discard_tracing().await;
            None
        }
    };
    let _ = fixtures.context.close().await;
    // A browser that went away under the test makes any failure meaningless, however the test
    // reported it: an assertion on an error it expected (`unwrap_err()` on a click that should
    // time out) sees the disconnect instead, and panics. Such a failure is retried like any
    // other loss of the browser.
    let browser_died = failed && !fixtures.browser.is_connected();
    let attempt = match outcome {
        Ok(r) => match r.outcome() {
            // A test that passed but left accessibility errors behind fails, in enforce mode.
            Ok(()) => match a11y_failure {
                Some(message) => Attempt::Failed {
                    message,
                    browser_lost: false,
                },
                None => Attempt::Passed,
            },
            Err(message) => Attempt::Failed {
                message,
                browser_lost: r.browser_lost() || browser_died,
            },
        },
        Err(panic) if browser_died => Attempt::Failed {
            message: format!(
                "{} (the browser disconnected during the test)",
                panic_message(panic.as_ref())
            ),
            browser_lost: true,
        },
        Err(panic) => Attempt::Panicked(panic),
    };
    (
        attempt,
        Artifacts {
            screenshot,
            trace,
            accessibility,
        },
    )
}

/// The text of a panic payload, as the default hook prints it.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "the test panicked".to_owned()
    }
}

enum Attempt {
    Passed,
    SetupFailed(Error),
    Failed { message: String, browser_lost: bool },
    Panicked(Box<dyn Any + Send>),
}

async fn setup(options: &TestOptions) -> Result<Fixtures> {
    let browser = shared_browser().await?;
    let context = browser.new_context((options.context)()).await?;
    let page = match context.new_page().await {
        Ok(p) => p,
        Err(e) => {
            let _ = context.close().await;
            return Err(e);
        }
    };
    Ok(Fixtures {
        browser,
        context,
        page,
    })
}

/// Saves a screenshot of a failed test's page when `failure.screenshot` is configured, and
/// returns where.
async fn capture_failure(name: &str, page: &Page) -> Option<std::path::PathBuf> {
    let failure = &config::get().failure;
    if failure.screenshot != Some(true) || page.is_closed() {
        return None;
    }
    let dir = failure
        .screenshot_path
        .clone()
        .unwrap_or_else(|| "target/agito-failures".into());
    let png = page.screenshot(ScreenshotOptions::default()).await.ok()?;
    let file = std::path::Path::new(&dir).join(format!("{}.png", artifact_name(name)));
    if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&file, png).is_ok() {
        eprintln!("agito: failure screenshot saved to {}", file.display());
        return Some(std::path::absolute(&file).unwrap_or(file));
    }
    None
}

/// Saves the trace of a failed test when `failure.trace` is configured, and returns where.
async fn save_failure_trace(
    name: &str,
    context: &crate::BrowserContext,
) -> Option<std::path::PathBuf> {
    let dir = config::get()
        .failure
        .trace_path
        .clone()
        .unwrap_or_else(|| "target/agito-failures".into());
    let file = std::path::Path::new(&dir).join(format!("{}.trace.zip", artifact_name(name)));
    match context.stop_tracing(&file).await {
        Ok(()) => {
            eprintln!("agito: failure trace saved to {}", file.display());
            Some(std::path::absolute(&file).unwrap_or(file))
        }
        Err(e) => {
            eprintln!("agito: the failure trace could not be saved: {e}");
            None
        }
    }
}

/// A file-name-safe version of a test name.
pub fn artifact_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.len() > 200 {
        cleaned[..200].to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes() {
        assert!(().outcome().is_ok());
        let lost: Result<()> = Err(Error::target_closed("page", None, "gone"));
        assert!(lost.browser_lost());
        assert_eq!(lost.outcome().unwrap_err(), "gone");
        let other: std::result::Result<(), String> = Err("x".into());
        assert!(!other.browser_lost());
        assert_eq!(other.outcome().unwrap_err(), "\"x\"");
    }

    #[test]
    fn artifact_names() {
        assert_eq!(artifact_name("tests::login works"), "tests__login_works");
    }
}

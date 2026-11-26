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

use crate::browser::Browser;
use crate::config;
use crate::context::{BrowserContext, ContextOptions};
use crate::error::{Error, Result};
use crate::launch::{LaunchOptions, launch};
use crate::page::{Page, ScreenshotOptions};

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
    pub browser: Browser,
    pub context: BrowserContext,
    pub page: Page,
}

/// Per-test settings from the attribute.
#[derive(Clone)]
pub struct TestOptions {
    /// Re-runs after a lost browser.
    pub retries: u32,
    pub context: fn() -> ContextOptions,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            retries: 1,
            context: ContextOptions::default,
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
/// Panics (failing the test) when the body fails.
pub fn run<F, Fut, R>(name: &str, options: TestOptions, body: F)
where
    F: Fn(Fixtures) -> Fut,
    Fut: Future<Output = R>,
    R: TestOutcome,
{
    let attempts = options.retries + 1;
    for attempt in 1..=attempts {
        let last_attempt = attempt == attempts;
        let result = RUNTIME.block_on(async {
            let _slot = CONTEXT_SLOTS
                .acquire()
                .await
                .expect("semaphore is never closed");
            let fixtures = match setup(&options).await {
                Ok(f) => f,
                Err(e) => return Attempt::SetupFailed(e),
            };
            let outcome = AssertUnwindSafe(body(fixtures.clone()))
                .catch_unwind()
                .await;
            let failed = !matches!(&outcome, Ok(r) if r.outcome().is_ok());
            if failed {
                capture_failure(name, &fixtures.page).await;
            }
            let _ = fixtures.context.close().await;
            match outcome {
                Ok(r) => match r.outcome() {
                    Ok(()) => Attempt::Passed,
                    Err(message) => Attempt::Failed {
                        message,
                        browser_lost: r.browser_lost(),
                    },
                },
                Err(panic) => Attempt::Panicked(panic),
            }
        });
        match result {
            Attempt::Passed => return,
            Attempt::SetupFailed(e) if e.is_browser_lost() && !last_attempt => {
                eprintln!(
                    "[RETRY] {name}: browser lost during setup ({e}); attempt {} of {attempts}",
                    attempt + 1
                );
            }
            Attempt::SetupFailed(e) => panic!("{name}: could not set up the browser: {e}"),
            Attempt::Failed {
                message,
                browser_lost: true,
            } if !last_attempt => {
                eprintln!(
                    "[RETRY] {name}: browser lost ({message}); attempt {} of {attempts}",
                    attempt + 1
                );
            }
            Attempt::Failed { message, .. } => panic!("{message}"),
            Attempt::Panicked(panic) => std::panic::resume_unwind(panic),
        }
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

/// Saves a screenshot of a failed test's page when `failure.screenshot` is configured.
async fn capture_failure(name: &str, page: &Page) {
    let failure = &config::get().failure;
    if failure.screenshot != Some(true) || page.is_closed() {
        return;
    }
    let dir = failure
        .screenshot_path
        .clone()
        .unwrap_or_else(|| "target/agito-failures".into());
    let Ok(png) = page.screenshot(ScreenshotOptions::default()).await else {
        return;
    };
    let file = std::path::Path::new(&dir).join(format!("{}.png", artifact_name(name)));
    if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&file, png).is_ok() {
        eprintln!("agito: failure screenshot saved to {}", file.display());
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

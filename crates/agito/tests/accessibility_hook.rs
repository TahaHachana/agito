//! Automatic accessibility audits, and the harness failing a test on them in enforce mode.
//! Run with `--features integration`.
#![cfg(feature = "integration")]

use agito::accessibility::{AccessibilityMode, AccessibilityOptions};
use agito::harness::{Fixtures, TestOptions};
use agito::{BrowserContext, ContextOptions, Page};

/// No alt text on the image: an error. No main landmark: a warning.
const BROKEN: &str = "data:text/html,<html lang=en><img src='data:,'><p>text</p></html>";
/// Only the missing main landmark: a warning.
const WARNING_ONLY: &str = "data:text/html,<html lang=en><h1>Fine</h1></html>";
const CLEAN: &str = "data:text/html,<html lang=en><main><h1>Fine</h1></main></html>";

fn with(mode: AccessibilityMode, tweak: impl Fn(&mut AccessibilityOptions)) -> ContextOptions {
    let mut accessibility = AccessibilityOptions::off();
    accessibility.mode = mode;
    tweak(&mut accessibility);
    ContextOptions {
        accessibility,
        ..ContextOptions::default()
    }
}

fn warn() -> ContextOptions {
    with(AccessibilityMode::Warn, |_| {})
}

fn warn_after_actions() -> ContextOptions {
    with(AccessibilityMode::Warn, |o| o.audit_after_actions = true)
}

fn rule_ids(context: &BrowserContext) -> Vec<String> {
    let mut ids: Vec<String> = context
        .accessibility_violations()
        .into_iter()
        .map(|v| v.rule_id)
        .collect();
    ids.sort();
    ids
}

#[agito::test(context = warn)]
async fn warn_mode_records_what_each_navigation_finds(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    page.goto(CLEAN).await?;
    assert!(context.accessibility_violations().is_empty());
    page.goto(BROKEN).await?;
    assert_eq!(
        rule_ids(&context),
        ["a11y-alt-text", "a11y-missing-landmark"]
    );
    // The same page again finds nothing new: each violation is kept once.
    page.goto(BROKEN).await?;
    assert_eq!(context.accessibility_violations().len(), 2);
    Ok(())
}

#[agito::test(context = warn)]
async fn actions_are_not_audited_unless_asked(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    page.goto(ADDS_A_FIELD).await?;
    page.locator("#add").click().await?;
    assert!(context.accessibility_violations().is_empty());
    Ok(())
}

/// Clicking the button adds an input with no label.
const ADDS_A_FIELD: &str = "data:text/html,<html lang=en><main><button id=add \
    onclick=\"document.querySelector('main').append(document.createElement('input'))\">Add</button></main></html>";

#[agito::test(context = warn_after_actions)]
async fn actions_that_change_the_page_are_audited_when_asked(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    page.goto(ADDS_A_FIELD).await?;
    assert!(context.accessibility_violations().is_empty());
    page.locator("#add").click().await?;
    assert_eq!(rule_ids(&context), ["a11y-unlabeled-form-control"]);
    Ok(())
}

fn enforce() -> ContextOptions {
    with(AccessibilityMode::Enforce, |_| {})
}

fn enforce_errors_only() -> ContextOptions {
    with(AccessibilityMode::Enforce, |o| o.include_warnings = false)
}

fn enforce_skipping_alt_text() -> ContextOptions {
    with(AccessibilityMode::Enforce, |o| {
        o.skip_rules = vec!["a11y-alt-text".into(), "a11y-missing-landmark".into()]
    })
}

/// Runs a test the way `#[agito::test]` does, and returns its failure message, if it failed.
fn run_test(context: fn() -> ContextOptions, url: &'static str) -> Option<String> {
    let outcome = std::panic::catch_unwind(|| {
        agito::harness::run(
            "accessibility_hook::enforced",
            TestOptions {
                retries: 0,
                context,
                quarantine: None,
            },
            |fixtures: Fixtures| async move {
                fixtures.page.goto(url).await?;
                Ok::<(), agito::Error>(())
            },
        )
    });
    outcome.err().map(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "a panic without a message".into())
    })
}

#[test]
fn enforce_mode_fails_a_test_that_leaves_violations() {
    let message = run_test(enforce, BROKEN).expect("the test fails");
    assert!(
        message.contains("2 accessibility violation(s), in enforce mode"),
        "{message}"
    );
    assert!(message.contains("[error] a11y-alt-text"), "{message}");
    assert!(
        message.contains("[warning] a11y-missing-landmark"),
        "{message}"
    );
    assert_eq!(run_test(enforce, CLEAN), None);
}

#[test]
fn enforce_mode_can_let_warnings_through_and_skip_rules() {
    assert_eq!(run_test(enforce_errors_only, WARNING_ONLY), None);
    let message = run_test(enforce_errors_only, BROKEN).expect("the error still fails it");
    assert!(
        message.contains("1 accessibility violation(s)"),
        "{message}"
    );
    assert_eq!(run_test(enforce_skipping_alt_text, BROKEN), None);
}

#[test]
fn a_failing_test_keeps_its_own_failure() {
    let outcome = std::panic::catch_unwind(|| {
        agito::harness::run(
            "accessibility_hook::own_failure",
            TestOptions {
                retries: 0,
                context: enforce,
                quarantine: None,
            },
            |fixtures: Fixtures| async move {
                fixtures.page.goto(BROKEN).await?;
                Err::<(), _>(agito::Error::from(agito::ErrorKind::Other(
                    "the checkout broke".into(),
                )))
            },
        )
    });
    let message = outcome
        .unwrap_err()
        .downcast_ref::<String>()
        .cloned()
        .unwrap();
    assert_eq!(message, "the checkout broke");
}

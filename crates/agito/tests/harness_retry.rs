//! The harness re-runs a test whose browser went away under it, even when the test panicked
//! rather than returning the error. Alone in its binary, since it closes the shared browser.
#![cfg(feature = "integration")]

use std::sync::atomic::{AtomicU32, Ordering};

use agito::{Browser, Page};

static ATTEMPTS: AtomicU32 = AtomicU32::new(0);

#[agito::test]
async fn a_panic_after_the_browser_died_is_retried(
    browser: Browser,
    page: Page,
) -> agito::Result<()> {
    if ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
        // As when Chrome crashes: the browser goes, and an assertion on an expected error sees
        // the disconnect instead.
        browser.close().await?;
        let err = page.set_content("<p>x</p>").await.unwrap_err();
        assert!(
            err.to_string().contains("expected an error about the page"),
            "{err}"
        );
    }
    page.set_content("<p>second attempt</p>").await?;
    assert_eq!(ATTEMPTS.load(Ordering::SeqCst), 2);
    Ok(())
}

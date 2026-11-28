//! A page with an accessibility error, for `agito run --a11y`.
#![cfg(feature = "browser")]

use agito::Page;

#[agito::test]
async fn visits_a_page_with_an_unlabelled_image(page: Page) -> agito::Result<()> {
    page.goto("data:text/html,<html lang=en><main><img src='data:,'></main></html>")
        .await?;
    Ok(())
}

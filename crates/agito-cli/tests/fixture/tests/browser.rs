//! `#[agito::test]`s, which report through the harness's records.
#![cfg(feature = "browser")]

use agito::Page;

#[agito::test]
async fn loads(page: Page) -> agito::Result<()> {
    page.set_content("<p>ok</p>").await
}

#[agito::test]
async fn fails_with_an_error(_page: Page) -> agito::Result<()> {
    Err(agito::ErrorKind::Other("the checkout button never appeared".into()).into())
}

#[agito::test(quarantine = "tracked in issue 12")]
async fn quarantined(_page: Page) -> agito::Result<()> {
    Err(agito::ErrorKind::Other("still broken".into()).into())
}

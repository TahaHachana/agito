//! Filling, typing, pressing, checking and selecting. Run with `--features integration`.
#![cfg(feature = "integration")]

use agito::{Page, expect};
use agito_samples::{LOGIN_FORM, set_page_content};

#[agito::test]
async fn fill_populates_text_field(page: Page) -> agito::Result<()> {
    set_page_content(&page, LOGIN_FORM).await?;
    let email = page.get_by_label("Email");
    email.fill("alice@example.com").await?;
    expect(&email).to_have_value("alice@example.com").await
}

#[agito::test]
async fn type_text_simulates_keystrokes(page: Page) -> agito::Result<()> {
    set_page_content(&page, LOGIN_FORM).await?;
    let password = page.get_by_label("Password");
    password.type_text("secret123").await?;
    expect(&password).to_have_value("secret123").await
}

#[agito::test]
async fn press_submits_form(page: Page) -> agito::Result<()> {
    set_page_content(&page, LOGIN_FORM).await?;
    page.get_by_label("Email").fill("bob@example.com").await?;
    page.get_by_label("Email").press("Enter").await?;
    let feedback = page.locator("#feedback");
    expect(&feedback).to_be_visible().await?;
    expect(&feedback)
        .to_have_text("Welcome, bob@example.com!")
        .await
}

#[agito::test]
async fn check_toggles_checkbox(page: Page) -> agito::Result<()> {
    set_page_content(&page, LOGIN_FORM).await?;
    let remember = page.locator("#remember");
    remember.check().await?;
    expect(&remember).to_be_checked().await
}

#[agito::test]
async fn select_option_chooses_value(page: Page) -> agito::Result<()> {
    set_page_content(&page, LOGIN_FORM).await?;
    let role = page.get_by_label("Role");
    role.select_option(&["admin"]).await?;
    expect(&role).to_have_value("admin").await
}

#[agito::test]
async fn clear_empties_input(page: Page) -> agito::Result<()> {
    set_page_content(&page, LOGIN_FORM).await?;
    let email = page.get_by_label("Email");
    email.fill("filled@example.com").await?;
    expect(&email).to_have_value("filled@example.com").await?;
    email.clear().await?;
    expect(&email).to_be_empty().await
}

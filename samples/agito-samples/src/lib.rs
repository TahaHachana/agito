//! Fixtures and page objects shared by the sample tests.

use agito::{Locator, Page};
use base64::Engine;

/// Loads `html` through a real navigation to a `data:` URL, so lifecycle events fire and the
/// page is laid out by the time this returns.
pub async fn set_page_content(page: &Page, html: &str) -> agito::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(html);
    page.goto(&format!("data:text/html;base64,{encoded}"))
        .await?;
    Ok(())
}

/// A todo list: add, complete, clear completed, active count.
pub const TODO_APP: &str = include_str!("../fixtures/todo.html");

/// A login form: email, password, remember-me, role select, feedback.
pub const LOGIN_FORM: &str = include_str!("../fixtures/login.html");

/// The page object for [`TODO_APP`].
pub struct TodoAppPage {
    page: Page,
}

impl TodoAppPage {
    pub fn new(page: Page) -> Self {
        Self { page }
    }

    pub fn new_todo_input(&self) -> Locator {
        self.page.get_by_placeholder("What needs to be done?")
    }

    pub fn add_button(&self) -> Locator {
        self.page.get_by_role("button", Some("Add"))
    }

    pub fn todo_items(&self) -> Locator {
        self.page.locator(".todo-item")
    }

    pub fn clear_completed_button(&self) -> Locator {
        self.page.locator("#clear-completed")
    }

    pub fn active_count_badge(&self) -> Locator {
        self.page.get_by_test_id("active-count")
    }

    pub async fn add_todo(&self, text: &str) -> agito::Result<()> {
        self.new_todo_input().fill(text).await?;
        self.add_button().click().await
    }

    pub async fn complete_todo(&self, index: usize) -> agito::Result<()> {
        self.todo_items()
            .nth(index)
            .locator("input[type='checkbox']")?
            .check()
            .await
    }

    pub async fn all_todo_texts(&self) -> agito::Result<Vec<String>> {
        self.todo_items().locator("span")?.all_inner_texts().await
    }
}

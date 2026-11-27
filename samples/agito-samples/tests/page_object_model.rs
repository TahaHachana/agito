//! The todo page object in use. Run with `cargo test -p agito-samples --features integration`.
#![cfg(feature = "integration")]

use agito::{Page, expect};
use agito_samples::{TODO_APP, TodoAppPage, set_page_content};

async fn todo(page: &Page) -> agito::Result<TodoAppPage> {
    set_page_content(page, TODO_APP).await?;
    Ok(TodoAppPage::new(page.clone()))
}

#[agito::test]
async fn add_todo_appears_in_list(page: Page) -> agito::Result<()> {
    let todo = todo(&page).await?;
    todo.add_todo("Buy milk").await?;
    assert!(
        todo.all_todo_texts()
            .await?
            .contains(&"Buy milk".to_owned())
    );
    Ok(())
}

// ANCHOR: page_object_test
#[agito::test]
async fn complete_todo_updates_active_count(page: Page) -> agito::Result<()> {
    let todo = todo(&page).await?;
    todo.add_todo("Walk the dog").await?;
    expect(&todo.active_count_badge())
        .to_have_text("1 items left")
        .await?;
    todo.complete_todo(0).await?;
    expect(&todo.active_count_badge())
        .to_have_text("0 items left")
        .await?;
    Ok(())
}
// ANCHOR_END: page_object_test

#[agito::test]
async fn clear_completed_removes_finished_items(page: Page) -> agito::Result<()> {
    let todo = todo(&page).await?;
    todo.add_todo("Task A").await?;
    todo.add_todo("Task B").await?;
    todo.complete_todo(0).await?;
    todo.clear_completed_button().click().await?;
    expect(&todo.todo_items()).to_have_count(1).await?;
    expect(&todo.todo_items().first())
        .to_contain_text("Task B")
        .await?;
    Ok(())
}

#[agito::test]
async fn multiple_todos_maintain_order(page: Page) -> agito::Result<()> {
    let todo = todo(&page).await?;
    for text in ["First", "Second", "Third"] {
        todo.add_todo(text).await?;
    }
    assert_eq!(todo.all_todo_texts().await?, ["First", "Second", "Third"]);
    Ok(())
}

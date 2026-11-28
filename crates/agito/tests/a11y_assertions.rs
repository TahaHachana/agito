//! `to_have_role`, `to_have_accessible_name` and `to_have_accessible_description`. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

use std::time::Duration;

use agito::{Page, expect};
use regex::Regex;

const PAGE: &str = r#"
<nav id="nav">Main</nav>
<h2 id="heading">Orders</h2>
<a id="link" href="/home">Home</a>
<label>Email <input id="email"></label>
<input id="agree" type="checkbox" aria-label="I agree">
<img id="logo" alt="Shop logo" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=">
<div id="tab" role="tab">Details</div>
<div id="decoration" role="presentation">~</div>
<span id="hidden" aria-hidden="true">secret</span>
<button id="close" aria-label="Close dialog">×</button>
<button id="unnamed"><svg width="10" height="10"></svg></button>
<span id="title-label">Billing address</span>
<section id="billing" aria-labelledby="title-label">...</section>
<button id="pay" aria-describedby="pay-help">Pay</button>
<span id="pay-help">Charges your card now</span>
"#;

#[agito::test]
async fn roles_are_the_computed_ones(page: Page) -> agito::Result<()> {
    page.set_content(PAGE).await?;
    for (id, role) in [
        ("nav", "navigation"),
        ("heading", "heading"),
        ("link", "link"),
        ("email", "textbox"),
        ("agree", "checkbox"),
        ("tab", "tab"),
        ("close", "button"),
        ("billing", "region"),
    ] {
        expect(&page.locator(&format!("#{id}")))
            .to_have_role(role)
            .await?;
    }
    // ARIA's spellings work, whichever Chrome uses.
    let logo = page.locator("#logo");
    expect(&logo).to_have_role("img").await?;
    expect(&logo).to_have_role("image").await?;
    for id in ["decoration", "hidden"] {
        let element = page.locator(&format!("#{id}"));
        expect(&element).to_have_role("none").await?;
        expect(&element).to_have_role("presentation").await?;
    }
    expect(&page.locator("#link"))
        .not()
        .to_have_role("button")
        .await?;
    Ok(())
}

#[agito::test]
async fn names_come_from_wherever_the_browser_finds_them(page: Page) -> agito::Result<()> {
    page.set_content(PAGE).await?;
    for (id, name) in [
        ("email", "Email"),             // a wrapping label
        ("agree", "I agree"),           // aria-label
        ("logo", "Shop logo"),          // alt text
        ("billing", "Billing address"), // aria-labelledby
        ("close", "Close dialog"),      // aria-label over the visible text
        ("link", "Home"),               // its own text
    ] {
        expect(&page.locator(&format!("#{id}")))
            .to_have_accessible_name(name)
            .await?;
    }
    // A button with only an icon has no name: the accessibility bug this catches.
    expect(&page.locator("#unnamed"))
        .to_have_accessible_name("")
        .await?;
    expect(&page.locator("#close"))
        .to_have_accessible_name(Regex::new("^Close").unwrap())
        .await?;
    Ok(())
}

#[agito::test]
async fn descriptions_come_from_aria_describedby(page: Page) -> agito::Result<()> {
    page.set_content(PAGE).await?;
    expect(&page.locator("#pay"))
        .to_have_accessible_description("Charges your card now")
        .await?;
    expect(&page.locator("#close"))
        .to_have_accessible_description("")
        .await?;
    Ok(())
}

#[agito::test]
async fn assertions_wait_for_the_tree_to_change(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<div id="w">Loading</div>
        <script>
          setTimeout(() => {
            const w = document.querySelector('#w');
            // A button takes its name from its text; an alert, for one, would not.
            w.setAttribute('role', 'button');
            w.textContent = 'Saved';
          }, 300);
        </script>"#,
    )
    .await?;
    let widget = page.locator("#w");
    expect(&widget).to_have_role("button").await?;
    expect(&widget).to_have_accessible_name("Saved").await?;
    Ok(())
}

#[agito::test]
async fn failures_say_what_was_found(page: Page) -> agito::Result<()> {
    page.set_content(PAGE).await?;
    let err = expect(&page.locator("#close"))
        .with_timeout(Duration::from_millis(300))
        .to_have_role("link")
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("to_have_role"), "{message}");
    assert!(message.contains("Received: button"), "{message}");

    let err = expect(&page.locator("#missing"))
        .with_timeout(Duration::from_millis(300))
        .to_have_accessible_name("Anything")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("<element not found>"), "{err}");
    Ok(())
}

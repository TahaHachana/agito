//! Accessibility snapshots and audits. Run with `--features integration`.
#![cfg(feature = "integration")]

use std::sync::Arc;

use agito::accessibility::{AccessibilityRule, AuditOptions, AuditedDocument, Severity, Violation};
use agito::plugin::{Plugin, PluginContext};
use agito::{BrowserContext, FulfillOptions, LaunchOptions, Page, expect};
use async_trait::async_trait;

const ACCESSIBLE: &str = r#"<!doctype html><html lang="en"><title>Shop</title>
<header><nav><a href="/">Home</a> <a href="/orders">Orders</a></nav></header>
<main>
  <h1>Shop</h1>
  <h2>Search</h2>
  <form><label>Query <input name="q"></label> <button>Search</button></form>
  <img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" alt="Sale banner">
  <p style="color: #767676">Prices include tax.</p>
  <div style="background: rgb(20, 20, 20)"><p style="color: white">Dark panel text</p></div>
</main></html>"#;

const BROKEN: &str = r#"<!doctype html><html><title>Broken</title>
<h1>Title</h1>
<h3>Skipped a level</h3>
<img id="logo" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=">
<input id="q">
<button id="icon"><svg width="10" height="10"></svg></button>
<a id="empty-link" href="/x"></a>
<p id="faint" style="color: #bbbbbb">Hard to read</p>
<span id="dup">one</span><span id="dup">two</span>
</html>"#;

async fn rule_ids(page: &Page) -> agito::Result<Vec<String>> {
    let mut ids: Vec<String> = page
        .audit_accessibility(&AuditOptions::default())
        .await?
        .violations
        .into_iter()
        .map(|v| v.rule_id)
        .collect();
    ids.sort();
    Ok(ids)
}

#[agito::test]
async fn an_accessible_page_passes(page: Page) -> agito::Result<()> {
    page.set_content(ACCESSIBLE).await?;
    let result = page.audit_accessibility(&AuditOptions::default()).await?;
    assert!(result.violations.is_empty(), "{:#?}", result.violations);
    assert_eq!(result.documents, 1);
    assert_eq!(result.rules.len(), 9);
    assert!(result.nodes > 10);
    expect(&page).to_pass_accessibility_audit().await?;
    Ok(())
}

#[agito::test]
async fn each_rule_finds_its_problem_at_its_element(page: Page) -> agito::Result<()> {
    page.set_content(BROKEN).await?;
    let result = page.audit_accessibility(&AuditOptions::default()).await?;
    let found = |rule: &str| -> Vec<&Violation> {
        result
            .violations
            .iter()
            .filter(|v| v.rule_id == rule)
            .collect()
    };
    let at = |rule: &str| found(rule)[0].selector.clone().unwrap_or_default();

    assert_eq!(at("a11y-alt-text"), "#logo");
    assert_eq!(at("a11y-unlabeled-form-control"), "#q");
    assert_eq!(at("a11y-empty-button"), "#icon");
    assert_eq!(at("a11y-empty-link"), "#empty-link");
    assert_eq!(at("a11y-color-contrast"), "#faint");
    assert_eq!(at("a11y-duplicate-id"), "[id=\"dup\"]");
    assert_eq!(at("a11y-missing-lang"), "html");
    let heading = found("a11y-heading-hierarchy");
    assert_eq!(heading.len(), 1);
    assert_eq!(heading[0].severity, Severity::Warning);
    assert_eq!(heading[0].name.as_deref(), Some("Skipped a level"));
    assert_eq!(
        found("a11y-missing-landmark")[0].severity,
        Severity::Warning
    );
    assert!(
        found("a11y-color-contrast")[0]
            .message
            .contains("\"Hard to read\"")
    );
    assert_eq!(result.violations.len(), 9, "{:#?}", result.violations);
    Ok(())
}

#[agito::test]
async fn default_and_inherited_backgrounds_are_not_misread(page: Page) -> agito::Result<()> {
    // Motus read a transparent background as black, which failed most text on most pages.
    page.set_content(
        r#"<html lang="en"><main>
        <p>Plain text</p><a href="/x">A default link</a>
        <section style="background: #123"><div><span style="color: #eee">Light on dark, through a transparent div</span></div></section>
        <p style="color: #767676; background: rgba(0,0,0,0)">Grey that just passes</p>
        <div style="background-image: linear-gradient(red, blue)"><span style="color: #888">On a gradient: not judged</span></div>
        </main></html>"#,
    )
    .await?;
    let result = page.audit_accessibility(&AuditOptions::default()).await?;
    assert!(
        result
            .violations
            .iter()
            .all(|v| v.rule_id != "a11y-color-contrast"),
        "{:#?}",
        result.violations
    );
    Ok(())
}

#[agito::test]
async fn iframes_are_audited_without_page_wide_rules(
    context: BrowserContext,
    page: Page,
) -> agito::Result<()> {
    context
        .route("https://widget.test/**", |route| async move {
            // No lang and no main: fine for an embedded document.
            let _ = route
                .fulfill(FulfillOptions::html("<input id=\"card\">"))
                .await;
        })
        .await?;
    context
        .route("https://shop.test/**", |route| async move {
            let _ = route
                .fulfill(FulfillOptions::html(
                    r#"<html lang="en"><main><h1>Pay</h1><iframe title="Card" src="https://widget.test/"></iframe></main></html>"#,
                ))
                .await;
        })
        .await?;
    page.goto("https://shop.test/").await?;
    // The cross-origin iframe registers a moment after the page loads.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while page.frame_by_url("https://widget.test/").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the widget never loaded"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let result = page.audit_accessibility(&AuditOptions::default()).await?;
    assert_eq!(result.documents, 2);
    assert_eq!(result.violations.len(), 1, "{:#?}", result.violations);
    let violation = &result.violations[0];
    assert_eq!(violation.rule_id, "a11y-unlabeled-form-control");
    assert_eq!(violation.frame_url, "https://widget.test/");
    assert_eq!(violation.selector.as_deref(), Some("#card"));

    let err = expect(&page)
        .to_pass_accessibility_audit()
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("1 violation(s) on https://shop.test/"),
        "{err}"
    );
    assert!(err.contains("[error] a11y-unlabeled-form-control"), "{err}");
    assert!(
        err.contains("(#card) in frame https://widget.test/"),
        "{err}"
    );
    Ok(())
}

#[agito::test]
async fn rules_can_be_skipped_and_warnings_let_through(page: Page) -> agito::Result<()> {
    // Only warnings: no main landmark, and a skipped heading level.
    page.set_content(r#"<html lang="en"><h1>A</h1><h3>B</h3></html>"#)
        .await?;
    assert_eq!(
        rule_ids(&page).await?,
        ["a11y-heading-hierarchy", "a11y-missing-landmark"]
    );
    assert!(expect(&page).to_pass_accessibility_audit().await.is_err());
    expect(&page)
        .to_pass_accessibility_audit_with(&AuditOptions {
            include_warnings: false,
            ..AuditOptions::default()
        })
        .await?;
    expect(&page)
        .to_pass_accessibility_audit_with(&AuditOptions::skipping(&[
            "a11y-heading-hierarchy",
            "a11y-missing-landmark",
        ]))
        .await?;
    expect(&page).not().to_pass_accessibility_audit().await?;
    Ok(())
}

#[agito::test]
async fn the_snapshot_is_the_tree_assistive_technology_sees(page: Page) -> agito::Result<()> {
    page.set_content(ACCESSIBLE).await?;
    let snapshot = page.accessibility_snapshot().await?;
    let nodes: Vec<_> = snapshot.nodes().collect();
    assert_eq!(nodes[0].role, "RootWebArea");
    assert_eq!(nodes[0].name, "Shop");
    let names = |role: &str| -> Vec<&str> {
        nodes
            .iter()
            .filter(|n| n.role == role)
            .map(|n| n.name.as_str())
            .collect()
    };
    assert_eq!(names("link"), ["Home", "Orders"]);
    assert_eq!(names("textbox"), ["Query"]);
    assert_eq!(names("main").len(), 1);
    let h2 = nodes
        .iter()
        .find(|n| n.name == "Search" && n.role == "heading")
        .unwrap();
    assert_eq!(h2.properties.get("level").map(String::as_str), Some("2"));
    assert!(snapshot.ignored_count > 0, "layout wrappers are left out");
    Ok(())
}

/// Replaces the built-in empty-button rule with a lenient one, and adds a rule of its own.
struct HouseRules;

struct NoTitle;
impl AccessibilityRule for NoTitle {
    fn id(&self) -> &str {
        "house-no-title"
    }
    fn description(&self) -> &str {
        "Pages have a title."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        let titled = document
            .nodes()
            .next()
            .is_some_and(|root| !root.name.is_empty());
        if !document.is_top || titled {
            return Vec::new();
        }
        vec![Violation::new(
            self.id(),
            Severity::Error,
            "The page has no <title>.",
        )]
    }
}

struct LenientButtons;
impl AccessibilityRule for LenientButtons {
    fn id(&self) -> &str {
        "a11y-empty-button"
    }
    fn description(&self) -> &str {
        "Never fails."
    }
    fn check(&self, _: &AuditedDocument) -> Vec<Violation> {
        Vec::new()
    }
}

#[async_trait]
impl Plugin for HouseRules {
    fn id(&self) -> &str {
        "test.house-rules"
    }
    async fn on_loaded(&self, context: &mut PluginContext) -> agito::Result<()> {
        context.register_accessibility_rule(Arc::new(NoTitle));
        context.register_accessibility_rule(Arc::new(LenientButtons));
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn plugins_add_rules_and_replace_built_in_ones() -> agito::Result<()> {
    let browser = agito::launch(LaunchOptions {
        plugins: vec![Arc::new(HouseRules)],
        ..LaunchOptions::default()
    })
    .await?;
    let page = browser.new_page(Default::default()).await?;
    page.set_content(
        r#"<html lang="en"><main><button><svg width="9" height="9"></svg></button></main></html>"#,
    )
    .await?;
    let result = page.audit_accessibility(&AuditOptions::default()).await?;
    let ids: Vec<&str> = result
        .violations
        .iter()
        .map(|v| v.rule_id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["house-no-title"],
        "the empty button passes the replaced rule"
    );
    assert_eq!(result.rules.len(), 10);
    browser.close().await
}

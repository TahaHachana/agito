//! Browser-driving tests. Run with `cargo test -p agito --features integration`.
#![cfg(feature = "integration")]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agito::{BrowserContext, Cookie, ErrorKind, Page, ScreenshotOptions, expect};
use common::{html, serve};
use regex::Regex;

#[agito::test]
async fn navigates_and_reads_title(page: Page) -> agito::Result<()> {
    page.goto(&html("<h1>Hello</h1>")).await?;
    assert_eq!(page.title().await?, "t");
    expect(&page.locator("h1")).to_have_text("Hello").await?;
    Ok(())
}

#[agito::test]
async fn goto_returns_the_document_response(page: Page) -> agito::Result<()> {
    let base = serve(&[("/", 200, "<p>home</p>"), ("/gone", 410, "<p>gone</p>")]).await;
    let ok = page
        .goto(&base)
        .await?
        .expect("http navigation has a response");
    expect(&ok).to_be_ok()?;
    assert_eq!(ok.headers.get("x-agito").map(String::as_str), Some("yes"));
    let gone = page.goto(&format!("{base}/gone")).await?.unwrap();
    expect(&gone).to_have_status(410)?;
    expect(&page).to_have_url(format!("{base}/gone")).await?;
    expect(&page).to_have_url("**/gone").await?;
    Ok(())
}

#[agito::test]
async fn navigation_errors_are_reported(page: Page) -> agito::Result<()> {
    let err = page.goto("http://127.0.0.1:1/").await.unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::Navigation { .. }),
        "{err:?}"
    );
    Ok(())
}

#[agito::test]
async fn history_and_reload(page: Page) -> agito::Result<()> {
    let base = serve(&[("/a", 200, "<p>a</p>"), ("/b", 200, "<p>b</p>")]).await;
    page.goto(&format!("{base}/a")).await?;
    page.goto(&format!("{base}/b")).await?;
    page.go_back().await?;
    expect(&page).to_have_url("**/a").await?;
    page.go_forward().await?;
    expect(&page).to_have_url("**/b").await?;
    page.reload().await?;
    expect(&page.locator("p")).to_have_text("b").await?;
    Ok(())
}

#[agito::test]
async fn click_and_text_assertions(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<button id="b" onclick="document.getElementById('out').textContent = 'clicked ' + (++window.n)">Go</button>
           <div id="out"></div><script>window.n = 0</script>"#,
    )
    .await?;
    page.locator("#b").click().await?;
    expect(&page.locator("#out"))
        .to_have_text("clicked 1")
        .await?;
    page.locator("#b").dblclick().await?;
    expect(&page.locator("#out"))
        .to_have_text(Regex::new(r"clicked \d").unwrap())
        .await?;
    expect(&page.locator("#out"))
        .to_contain_text("clicked")
        .await?;
    Ok(())
}

#[agito::test]
async fn fill_type_press_and_values(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<form onsubmit="event.preventDefault(); document.getElementById('r').textContent = this.q.value">
             <label for="q">Query</label><input id="q" name="q" placeholder="Search here">
           </form><div id="r"></div>"#,
    )
    .await?;
    let input = page.get_by_label("Query");
    input.fill("hello").await?;
    expect(&input).to_have_value("hello").await?;
    input.clear().await?;
    page.get_by_placeholder("search")
        .type_text("Typed!")
        .await?;
    assert_eq!(input.input_value().await?, "Typed!");
    input.press("Enter").await?;
    expect(&page.locator("#r")).to_have_text("Typed!").await?;
    Ok(())
}

#[agito::test]
async fn selector_strategies(page: Page) -> agito::Result<()> {
    page.set_content(
        r##"<nav><a href="#x">Home</a></nav>
           <button data-testid="save">Save changes</button>
           <ul><li>one</li><li class="sel">two</li><li>three</li></ul>
           <img alt="Logo" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=">
           <span title="tip">hover me</span>"##,
    )
    .await?;
    expect(&page.get_by_role("button", Some("Save changes")))
        .to_be_visible()
        .await?;
    expect(&page.get_by_role("link", Some("Home")))
        .to_have_attribute("href", "#x")
        .await?;
    expect(&page.get_by_test_id("save"))
        .to_have_text("Save changes")
        .await?;
    expect(&page.locator("data-testid=save"))
        .to_be_visible()
        .await?;
    expect(&page.get_by_text("save CHANGES"))
        .to_have_count(1)
        .await?;
    expect(&page.get_by_text_exact("Save changes"))
        .to_have_count(1)
        .await?;
    expect(&page.locator("text=two"))
        .to_have_class("sel")
        .await?;
    expect(&page.locator("xpath=//li[3]"))
        .to_have_text("three")
        .await?;
    expect(&page.get_by_alt_text("logo"))
        .to_be_attached()
        .await?;
    expect(&page.get_by_title("tip"))
        .to_have_text("hover me")
        .await?;
    let items = page.locator("li");
    expect(&items).to_have_count(3).await?;
    expect(&items.first()).to_have_text("one").await?;
    expect(&items.last()).to_have_text("three").await?;
    expect(&items.nth(1)).to_have_text("two").await?;
    expect(&items.filter_has_text("thr"))
        .to_have_count(1)
        .await?;
    assert_eq!(items.all_text_contents().await?, ["one", "two", "three"]);
    assert_eq!(items.all().await?.len(), 3);
    Ok(())
}

#[agito::test]
async fn chained_and_parent_locators(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<div class="card"><h2>A</h2><button>Edit</button></div>
           <div class="card"><h2>B</h2><button>Edit</button></div>"#,
    )
    .await?;
    let card_b = page.locator("h2").filter_has_text("B").locator("..")?;
    expect(&card_b.locator("button")?).to_have_count(1).await?;
    expect(&page.locator(".card").locator("h2")?)
        .to_have_count(2)
        .await?;
    Ok(())
}

#[agito::test]
async fn shadow_dom_is_pierced(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<div id="host"></div><script>
             const root = document.getElementById('host').attachShadow({ mode: 'open' });
             root.innerHTML = '<button class="inner">Inside</button>';
           </script>"#,
    )
    .await?;
    expect(&page.locator(".inner"))
        .to_have_text("Inside")
        .await?;
    expect(&page.locator(".inner").pierce_shadow(false))
        .to_have_count(0)
        .await?;
    page.locator(".inner").click().await?;
    Ok(())
}

#[agito::test]
async fn checkboxes_and_selects(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<input type="checkbox" id="c"><input type="radio" name="r" id="r1"><input type="radio" name="r" id="r2">
           <select id="s"><option value="a">Apple</option><option value="b">Banana</option></select>"#,
    )
    .await?;
    let c = page.locator("#c");
    c.check().await?;
    expect(&c).to_be_checked().await?;
    c.uncheck().await?;
    expect(&c).not().to_be_checked().await?;
    page.locator("#r2").check().await?;
    expect(&page.locator("#r2")).to_be_checked().await?;
    assert!(page.locator("#r2").uncheck().await.is_err());
    assert_eq!(page.locator("#s").select_option(&["Banana"]).await?, ["b"]);
    expect(&page.locator("#s")).to_have_value("b").await?;
    Ok(())
}

#[agito::test]
async fn waits_for_elements_to_appear_and_disappear(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<div id="spinner">loading</div><script>
             setTimeout(() => { document.getElementById('spinner').remove();
               const b = document.createElement('button'); b.id = 'late'; b.textContent = 'Late';
               b.onclick = () => b.textContent = 'Done'; document.body.append(b); }, 400);
           </script>"#,
    )
    .await?;
    page.locator("#late").click().await?;
    expect(&page.locator("#late")).to_have_text("Done").await?;
    expect(&page.locator("#spinner")).to_be_hidden().await?;
    expect(&page.locator("#spinner")).to_be_detached().await?;
    page.locator("#spinner")
        .wait_for(agito::ElementState::Detached, None)
        .await?;
    Ok(())
}

#[agito::test]
async fn actionability_failures_say_why(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<button id="b" disabled>Nope</button><div id="h" style="display:none">x</div>"#,
    )
    .await?;
    let err = page
        .locator("#b")
        .with_timeout(Duration::from_millis(500))
        .click()
        .await
        .unwrap_err();
    match err.kind() {
        ErrorKind::ActionTimeout { failed_check, .. } => assert_eq!(failed_check, "enabled"),
        other => panic!("unexpected {other:?}"),
    }
    let err = page
        .locator("#h")
        .with_timeout(Duration::from_millis(500))
        .click()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not visible"), "{err}");
    let err = expect(&page.locator("#missing"))
        .with_timeout(Duration::from_millis(300))
        .to_be_visible()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("<element not found>"), "{err}");
    expect(&page.locator("#b")).to_be_disabled().await?;
    Ok(())
}

#[agito::test]
async fn covered_elements_are_not_clicked(page: Page) -> agito::Result<()> {
    page.set_content(
        r#"<div><button id="b" style="position:absolute;top:10px;left:10px">Under</button></div>
           <div id="cover" style="position:absolute;top:0;left:0;width:300px;height:100px;background:red"></div>"#,
    )
    .await?;
    let err = page
        .locator("#b")
        .with_timeout(Duration::from_millis(500))
        .click()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("div#cover"), "{err}");
    Ok(())
}

#[agito::test]
async fn evaluate_and_content(page: Page) -> agito::Result<()> {
    page.set_content("<p id='p'>x</p>").await?;
    let sum: i64 = page.evaluate("1 + 2").await?;
    assert_eq!(sum, 3);
    let n: i64 = page
        .evaluate("async () => { await new Promise(r => setTimeout(r, 10)); return 7; }")
        .await?;
    assert_eq!(n, 7);
    let doubled: Vec<i64> = page
        .evaluate_with("xs => xs.map(x => x * 2)", &vec![1, 2])
        .await?;
    assert_eq!(doubled, [2, 4]);
    let missing: Option<String> = page.evaluate("undefined").await?;
    assert!(missing.is_none());
    let err = page
        .evaluate::<i64>("(() => { throw new Error('boom') })()")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("boom"), "{err}");
    assert!(page.content().await?.contains("<p id=\"p\">x</p>"));
    let id: String = page
        .locator("#p")
        .evaluate("function() { return this.id; }", &[])
        .await?;
    assert_eq!(id, "p");
    Ok(())
}

#[agito::test]
async fn screenshots_and_pdf(page: Page) -> agito::Result<()> {
    page.set_content("<div style='height:3000px'>tall</div>")
        .await?;
    let png = page.screenshot(ScreenshotOptions::default()).await?;
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let full = page
        .screenshot(ScreenshotOptions {
            full_page: true,
            ..Default::default()
        })
        .await?;
    assert!(full.len() > 100);
    let pdf = page.pdf(Default::default()).await?;
    assert_eq!(&pdf[..4], b"%PDF");
    Ok(())
}

#[agito::test]
async fn console_errors_and_dialogs(page: Page) -> agito::Result<()> {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    page.on_console(move |m| sink.lock().unwrap().push(format!("{}:{}", m.kind, m.text)));
    let errors = Arc::new(Mutex::new(Vec::new()));
    let error_sink = errors.clone();
    page.on_page_error(move |e| error_sink.lock().unwrap().push(e.to_owned()));
    page.on_dialog(|d| async move {
        let _ = d.accept(Some("Ada")).await;
    });
    page.set_content("<div id='o'></div>").await?;
    page.evaluate::<()>("console.log('hi', 42)").await?;
    page.evaluate::<()>("void setTimeout(() => { throw new Error('kaboom') }, 0)")
        .await?;
    let answer: String = page.evaluate("prompt('name?')").await?;
    assert_eq!(answer, "Ada");
    page.wait_for_timeout(Duration::from_millis(200)).await;
    assert!(
        logs.lock().unwrap().contains(&"log:hi 42".to_owned()),
        "{:?}",
        logs.lock().unwrap()
    );
    assert!(errors.lock().unwrap().iter().any(|e| e.contains("kaboom")));
    Ok(())
}

#[agito::test]
async fn dialogs_are_dismissed_without_a_handler(page: Page) -> agito::Result<()> {
    let confirmed: bool = page.evaluate("confirm('sure?')").await?;
    assert!(!confirmed);
    Ok(())
}

#[agito::test]
async fn cookies_are_per_context(context: BrowserContext, page: Page) -> agito::Result<()> {
    let base = serve(&[("/", 200, "<p>c</p>")]).await;
    page.goto(&base).await?;
    context
        .add_cookies(&[Cookie::new("flavor", "oat", base.clone())])
        .await?;
    let seen: String = page.evaluate("document.cookie").await?;
    assert_eq!(seen, "flavor=oat");
    assert!(context.cookies().await?.iter().any(|c| c.name == "flavor"));
    let other = context.browser().new_context(Default::default()).await?;
    assert!(other.cookies().await?.is_empty());
    other.close().await?;
    context.clear_cookies().await?;
    assert!(context.cookies().await?.is_empty());
    Ok(())
}

#[agito::test]
async fn keyboard_and_mouse(page: Page) -> agito::Result<()> {
    page.set_content(r#"<textarea id="t"></textarea>"#).await?;
    page.locator("#t").focus().await?;
    page.keyboard().type_text("ab").await?;
    page.keyboard().press("Shift+KeyC").await?;
    page.keyboard().press("Backspace").await?;
    page.keyboard().type_text("Ω").await?;
    expect(&page.locator("#t")).to_have_value("abΩ").await?;
    Ok(())
}

#[agito::test]
async fn closed_pages_fail_as_browser_lost(page: Page) -> agito::Result<()> {
    page.close().await?;
    assert!(page.is_closed());
    let err = page.goto(&html("x")).await.unwrap_err();
    assert!(err.is_browser_lost());
    Ok(())
}

#[agito::test]
async fn wait_for_url_after_script_navigation(page: Page) -> agito::Result<()> {
    let base = serve(&[
        ("/", 200, "<a id='go' href='/next'>next</a>"),
        ("/next", 200, "<p>next</p>"),
    ])
    .await;
    page.goto(&base).await?;
    page.locator("#go").click().await?;
    page.wait_for_url("**/next", None).await?;
    page.wait_for_load_state(agito::WaitUntil::Load, None)
        .await?;
    expect(&page.locator("p")).to_have_text("next").await?;
    Ok(())
}

fn small_viewport() -> agito::ContextOptions {
    agito::ContextOptions {
        viewport: Some(agito::Viewport {
            width: 400,
            height: 300,
        }),
        ..Default::default()
    }
}

#[agito::test(context = small_viewport)]
async fn context_options_apply(page: Page) -> agito::Result<()> {
    let width: u32 = page.evaluate("window.innerWidth").await?;
    assert_eq!(width, 400);
    Ok(())
}

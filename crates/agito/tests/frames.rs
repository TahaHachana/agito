//! Iframes, same-process and out-of-process. Run with `--features integration`.
#![cfg(feature = "integration")]

mod common;

use std::time::Duration;

use agito::{Page, expect};
use common::serve;

const BUTTON: &str = r#"<button id="b" onclick="this.textContent = 'clicked'">press</button>
<input id="i" aria-label="Name"><p id="out"></p>"#;

fn srcdoc(html: &str) -> String {
    html.replace('&', "&amp;").replace('"', "&quot;")
}

#[agito::test]
async fn same_origin_iframe(page: Page) -> agito::Result<()> {
    page.set_content(&format!(
        r#"<h1>outer</h1><iframe id="f" name="inner" srcdoc="{}"></iframe>"#,
        srcdoc(BUTTON)
    ))
    .await?;
    let frame = page.frame_locator("#f");
    frame.locator("#b").click().await?;
    expect(&frame.locator("#b")).to_have_text("clicked").await?;
    frame.get_by_label("Name").fill("Ada").await?;
    expect(&frame.locator("#i")).to_have_value("Ada").await?;
    expect(&frame.get_by_role("button", Some("clicked")))
        .to_be_visible()
        .await?;
    // The outer document does not see into the frame.
    expect(&page.locator("#b")).to_have_count(0).await?;

    assert_eq!(page.frames().len(), 2);
    let inner = page.frame("inner").expect("frame by name");
    assert_eq!(inner.parent_frame(), Some(page.main_frame()));
    assert_eq!(page.main_frame().child_frames(), vec![inner.clone()]);
    let text: String = inner
        .evaluate("document.querySelector('#b').textContent")
        .await?;
    assert_eq!(text, "clicked");
    assert_eq!(frame.frame().await?, inner);
    Ok(())
}

#[agito::test]
async fn nested_iframes(page: Page) -> agito::Result<()> {
    let inner = format!(
        r#"<p>middle</p><iframe id="deep" srcdoc="{}"></iframe>"#,
        srcdoc(BUTTON)
    );
    page.set_content(&format!(
        r#"<iframe id="outer" srcdoc="{}"></iframe>"#,
        srcdoc(&inner)
    ))
    .await?;
    let deep = page.frame_locator("#outer").frame_locator("#deep");
    deep.locator("#b").click().await?;
    expect(&deep.locator("#b")).to_have_text("clicked").await?;
    expect(&page.frame_locator("#outer").locator("p"))
        .to_have_text("middle")
        .await?;
    assert_eq!(page.frames().len(), 3);
    Ok(())
}

#[agito::test]
async fn clicks_land_in_offset_and_scrolled_iframes(page: Page) -> agito::Result<()> {
    page.set_content(&format!(
        r#"<div style="height: 1500px">spacer</div>
           <iframe id="f" style="margin-left: 120px; border: 17px solid black; padding: 9px; width: 400px; height: 300px"
                   srcdoc="{}"></iframe>"#,
        srcdoc(r#"<div style="height: 600px"></div><button id="b" onclick="this.textContent='hit'">far down</button>"#)
    ))
    .await?;
    let button = page.frame_locator("#f").locator("#b");
    button.click().await?;
    expect(&button).to_have_text("hit").await?;
    // The box is reported in page viewport coordinates, inside the iframe's box.
    let b = button.bounding_box().await?.unwrap();
    let f = page.locator("#f").bounding_box().await?.unwrap();
    assert!(
        b.x > f.x && b.y > f.y && b.x + b.width < f.x + f.width,
        "{b:?} not inside {f:?}"
    );
    Ok(())
}

#[agito::test]
async fn cross_origin_iframe_runs_out_of_process(page: Page) -> agito::Result<()> {
    // The outer page is served from 127.0.0.1 and embeds the same server as localhost.
    // Those are different sites, so Chrome puts the iframe in its own process.
    let base = serve(&[
        ("/", 200, r#"<p>outer</p><iframe id="f" style="margin: 40px"></iframe>
            <script>document.getElementById('f').src = 'http://localhost:' + location.port + '/inner';</script>"#),
        ("/inner", 200, BUTTON),
    ])
    .await;
    page.goto(&base).await?;

    let frame = page.frame_locator("#f");
    frame.locator("#b").click().await?;
    expect(&frame.locator("#b")).to_have_text("clicked").await?;
    frame.locator("#i").fill("cross").await?;
    expect(&frame.locator("#i")).to_have_value("cross").await?;
    let inner = frame.frame().await?;
    let host: String = inner.evaluate("location.host").await?;
    assert!(host.starts_with("localhost:"), "{host}");
    assert!(inner.is_out_of_process());
    assert!(!page.main_frame().is_out_of_process());
    Ok(())
}

#[agito::test]
async fn frames_navigate_and_detach(page: Page) -> agito::Result<()> {
    let base = serve(&[
        ("/a", 200, "<p id='p'>a</p>"),
        ("/b", 200, "<p id='p'>b</p>"),
    ])
    .await;
    page.goto(&format!("{base}/a")).await?;
    page.evaluate::<()>(&format!(
        "new Promise(r => {{ const f = document.createElement('iframe'); f.name = 'kid'; f.src = '{base}/a'; f.onload = () => r(); document.body.append(f); }})"
    ))
    .await?;
    let kid = page.frame("kid").expect("attached frame");
    expect(&kid.locator("#p")).to_have_text("a").await?;
    kid.goto(&format!("{base}/b")).await?;
    expect(&kid.locator("#p")).to_have_text("b").await?;
    assert!(kid.url().ends_with("/b"));
    kid.wait_for_load_state(agito::WaitUntil::Load, None)
        .await?;

    page.evaluate::<()>("document.querySelector('iframe').remove()")
        .await?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !kid.is_detached() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(kid.is_detached());
    assert!(kid.evaluate::<i64>("1").await.is_err());
    assert_eq!(page.frames().len(), 1);
    Ok(())
}

#[agito::test]
async fn frame_locators_survive_iframe_replacement(page: Page) -> agito::Result<()> {
    page.set_content(&format!(
        r#"<div id="host"><iframe id="f" srcdoc="{}"></iframe></div>"#,
        srcdoc("<p>first</p>")
    ))
    .await?;
    let p = page.frame_locator("#f").locator("p");
    expect(&p).to_have_text("first").await?;
    page.evaluate::<()>(&format!(
        "void (document.getElementById('host').innerHTML = '<iframe id=\"f\" srcdoc=\"{}\"></iframe>')",
        "<p>second</p>"
    ))
    .await?;
    expect(&p).to_have_text("second").await?;
    Ok(())
}

#[agito::test]
async fn frame_locator_on_a_non_iframe_says_so(page: Page) -> agito::Result<()> {
    page.set_content("<div id='d'>x</div>").await?;
    let err = page
        .frame_locator("#d")
        .locator("p")
        .click()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("is not an <iframe>"), "{err}");
    Ok(())
}

#[agito::test]
async fn nested_out_of_process_iframes_below_the_fold(page: Page) -> agito::Result<()> {
    // 127.0.0.1 -> localhost -> 127.0.0.1: two site hops, so two out-of-process frames, the
    // outer one placed far below the viewport.
    let base = serve(&[
        ("/", 200, r#"<div style="height: 1600px"></div><iframe id="a" style="border: 5px solid; padding: 7px; width: 500px; height: 400px"></iframe>
            <script>document.getElementById('a').src = 'http://localhost:' + location.port + '/middle';</script>"#),
        ("/middle", 200, r#"<p>middle</p><div style="height: 250px"></div><iframe id="b" style="margin-left: 30px"></iframe>
            <script>document.getElementById('b').src = 'http://127.0.0.1:' + location.port + '/inner';</script>"#),
        ("/inner", 200, BUTTON),
    ])
    .await;
    page.goto(&base).await?;
    let deep = page.frame_locator("#a").frame_locator("#b");
    deep.locator("#b").click().await?;
    expect(&deep.locator("#b")).to_have_text("clicked").await?;
    let inner = deep.frame().await?;
    assert!(inner.is_out_of_process());
    assert!(inner.parent_frame().unwrap().is_out_of_process());
    Ok(())
}

#[agito::test]
async fn navigating_an_iframe_cross_site_moves_it_out_of_process(page: Page) -> agito::Result<()> {
    let base = serve(&[
        (
            "/",
            200,
            r#"<iframe id="f" name="kid" srcdoc="<p>local</p>"></iframe>"#,
        ),
        ("/inner", 200, BUTTON),
    ])
    .await;
    page.goto(&base).await?;
    let kid = page.frame("kid").expect("srcdoc frame");
    assert!(!kid.is_out_of_process());
    let port = base.rsplit(':').next().unwrap();
    kid.goto(&format!("http://localhost:{port}/inner")).await?;
    let frame = page.frame_locator("#f");
    frame.locator("#b").click().await?;
    expect(&frame.locator("#b")).to_have_text("clicked").await?;
    assert!(frame.frame().await?.is_out_of_process());
    Ok(())
}

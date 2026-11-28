//! Driving a browser agito did not launch. Run with `--features integration`.
#![cfg(feature = "integration")]

use std::process::Stdio;

use agito::{ConnectOptions, ContextOptions, expect};
use tokio::io::{AsyncBufReadExt, BufReader};

/// A browser started the way a person starts one to be driven: with a debugging port, open
/// at `url`. Killed when dropped.
struct RunningChrome {
    process: tokio::process::Child,
    profile: std::path::PathBuf,
    endpoint: String,
}

impl RunningChrome {
    async fn start(name: &str, url: &str) -> RunningChrome {
        let executable = agito::launch::finder::resolve(
            None,
            agito::LaunchOptions::default().executable_path.as_deref(),
        )
        .unwrap();
        let profile = std::env::temp_dir().join(format!("agito-{name}-{}", std::process::id()));
        let mut process = tokio::process::Command::new(executable)
            .args([
                "--headless=new",
                "--remote-debugging-port=0",
                "--no-first-run",
                "--no-sandbox",
                url,
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("chrome starts");
        let mut lines = BufReader::new(process.stderr.take().unwrap()).lines();
        let endpoint = loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("chrome printed its endpoint");
            if let Some(url) = line.strip_prefix("DevTools listening on ") {
                break url.trim().to_owned();
            }
        };
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        RunningChrome {
            process,
            profile,
            endpoint,
        }
    }
}

impl Drop for RunningChrome {
    fn drop(&mut self) {
        let _ = self.process.start_kill();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_drives_an_existing_browser() -> agito::Result<()> {
    let chrome = RunningChrome::start("connect", "about:blank").await;
    let endpoint = chrome.endpoint.clone();
    let browser = agito::connect(&endpoint, ConnectOptions::default()).await?;
    assert!(!browser.owns_process());
    let page = browser.new_page(ContextOptions::default()).await?;
    page.set_content(
        r#"<button onclick="window.open('').document.title = 'from opener'">open</button>"#,
    )
    .await?;
    let popup = page.wait_for_popup(None);
    page.locator("button").click().await?;
    let popup = popup.await?;
    expect(&popup).to_have_title("from opener").await?;
    assert_eq!(popup.opener().unwrap().target_id(), page.target_id());
    browser.disconnect().await?;
    drop(chrome);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_context_is_adopted_with_its_tabs_and_let_go_of() -> agito::Result<()> {
    let chrome = RunningChrome::start(
        "adopt",
        "data:text/html,<title>Mine</title><input id=q value=draft>\
         <button onclick=\"window.open('about:blank')\">open</button>",
    )
    .await;
    let browser = agito::connect(&chrome.endpoint, ConnectOptions::default()).await?;
    let context = browser.default_context(ContextOptions::default()).await?;
    let pages = context.pages();
    assert_eq!(pages.len(), 1, "the tab already open is adopted");
    let mine = &pages[0];
    assert_eq!(mine.title().await?, "Mine");
    // Driven as it is: the person's own state, and no emulated viewport.
    expect(&mine.locator("#q")).to_have_value("draft").await?;
    assert_eq!(
        browser
            .default_context(ContextOptions::default())
            .await?
            .id(),
        context.id(),
        "asking again returns the same context"
    );
    // A tab the page opens is adopted too. This browser blocks popups as any does, so it
    // takes a click.
    let popup = mine.wait_for_popup(None);
    mine.locator("button").click().await?;
    let _popup = popup.await?;
    assert_eq!(context.pages().len(), 2);

    // Letting go leaves the tabs open in the browser.
    context.close().await?;
    browser.disconnect().await?;
    let again = agito::connect(&chrome.endpoint, ConnectOptions::default()).await?;
    let readopted = again.default_context(ContextOptions::default()).await?;
    assert_eq!(readopted.pages().len(), 2);
    again.disconnect().await?;
    Ok(())
}

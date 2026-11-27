//! Driving a browser agito did not launch. Run with `--features integration`.
#![cfg(feature = "integration")]

use std::process::Stdio;

use agito::{ConnectOptions, ContextOptions, expect};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test(flavor = "multi_thread")]
async fn connect_drives_an_existing_browser() -> agito::Result<()> {
    let executable = agito::launch::finder::resolve(
        None,
        agito::LaunchOptions::default().executable_path.as_deref(),
    )?;
    let profile = std::env::temp_dir().join(format!("agito-connect-{}", std::process::id()));
    let mut chrome = tokio::process::Command::new(executable)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-sandbox",
            "about:blank",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("chrome starts");
    let mut lines = BufReader::new(chrome.stderr.take().unwrap()).lines();
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

    let _ = chrome.kill().await;
    let _ = std::fs::remove_dir_all(profile);
    Ok(())
}

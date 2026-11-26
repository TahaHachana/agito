//! Talks to a real Chrome over the raw transport. Ignored by default; run with
//! `AGITO_CHROME=/path/to/chrome cargo test -p agito --test live_transport -- --ignored`.
//! Superseded by the launcher's integration tests once Phase 2 lands.

use std::process::Stdio;

use agito::protocol::cdp::{browser, target};
use agito::transport::{CdpConnection, CdpSessionRegistry, ConnectionOptions, SessionRegistry, ws};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
#[ignore = "needs a Chrome binary"]
async fn chrome_answers_over_websocket() {
    let chrome = std::env::var("AGITO_CHROME").unwrap_or_else(|_| "google-chrome".into());
    let profile = std::env::temp_dir().join(format!("agito-live-{}", std::process::id()));
    let mut child = tokio::process::Command::new(chrome)
        .args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            // CI runners cannot start Chromium's sandbox for a browser outside its install path.
            "--no-sandbox",
        ])
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("chrome starts");
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
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

    let conn = CdpConnection::start(
        ws::connect(&endpoint).await.unwrap(),
        ConnectionOptions::default(),
    );
    let registry = CdpSessionRegistry::new(conn.clone());
    let browser_session = registry.browser_session();

    let version = browser_session
        .send(&browser::GetVersionParams::default())
        .await
        .unwrap();
    assert!(version.product.contains("Chrome"), "{}", version.product);

    let targets = browser_session
        .send(&target::GetTargetsParams::default())
        .await
        .unwrap();
    assert!(targets.target_infos.iter().any(|t| t.r#type == "page"));

    conn.close().await;
    let _ = child.kill().await;
    let _ = std::fs::remove_dir_all(profile);
}

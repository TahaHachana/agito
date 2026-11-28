//! The protocol log against a real browser. Run with `--features integration`.
#![cfg(feature = "integration")]

use std::sync::{Arc, Mutex};

use agito::transport::ProtocolLogOptions;
use agito::{ContextOptions, Cookie, FulfillOptions, LaunchOptions};

#[tokio::test(flavor = "multi_thread")]
async fn a_launch_logs_its_traffic_with_secrets_replaced() -> agito::Result<()> {
    let dir = std::env::temp_dir().join(format!("agito-protocol-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("nested/protocol.log");
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = lines.clone();

    let browser = agito::launch(LaunchOptions {
        protocol_log: Some(ProtocolLogOptions {
            sink: Some(Arc::new(move |line: &str| {
                seen.lock().unwrap().push(line.to_owned())
            })),
            ..ProtocolLogOptions::to(&path)
        }),
        ..LaunchOptions::default()
    })
    .await?;
    let context = browser.new_context(ContextOptions::default()).await?;
    let mut cookie = Cookie::new("session", "cookie-secret-value", "https://app.test");
    cookie.secure = true;
    context.add_cookies(&[cookie]).await?;
    context
        .route("https://app.test/**", |route| async move {
            let _ = route.fulfill(FulfillOptions::html("<p>app</p>")).await;
        })
        .await?;
    let page = context.new_page().await?;
    page.goto("https://app.test/").await?;
    // The page builds the token at run time, so it only reaches the log through the
    // browser's network events, which is what is being tested: the script's own source is
    // logged as it is, by design.
    page.evaluate::<()>(
        "fetch('/api', { headers: { Authorization: 'Bearer ' + ['header', 'secret', 'value'].join('-') } }).then(() => {})",
    )
    .await?;
    page.wait_for_network_idle(std::time::Duration::from_millis(200), None)
        .await?;
    browser.close().await?;

    let text = std::fs::read_to_string(&path).unwrap();
    let file_lines: Vec<&str> = text.lines().collect();
    assert!(
        file_lines[0].contains("protocol log, process"),
        "{}",
        file_lines[0]
    );
    assert!(
        file_lines[1].contains(" open cdp-pipe pid ") || file_lines[1].contains(" open cdp-ws "),
        "{}",
        file_lines[1]
    );
    assert!(
        text.contains(" -> #1 - Browser.getVersion {}"),
        "the first command"
    );
    assert!(text.contains(" <- #1 - Browser.getVersion "), "its answer");
    assert!(text.contains(" <- event "), "events from the browser");
    assert!(
        text.contains(r#""name":"session""#),
        "the cookie is logged by name"
    );
    for secret in ["cookie-secret-value", "header-secret-value"] {
        assert!(!text.contains(secret), "{secret} leaked into the log");
    }
    assert!(
        text.contains(r#""Authorization":"<redacted>""#),
        "the header is logged, with its value replaced"
    );
    assert!(
        file_lines.last().unwrap().ends_with(" closed"),
        "{}",
        file_lines.last().unwrap()
    );

    // The callback got the same lines.
    let sunk = lines.lock().unwrap();
    assert_eq!(
        sunk.len(),
        file_lines.len() - 1,
        "every line but the file's header"
    );
    assert!(sunk.iter().all(|l| !l.contains("cookie-secret-value")));

    std::fs::remove_dir_all(&dir).unwrap();
    Ok(())
}

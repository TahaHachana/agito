//! Launches a browser, opens a page and saves a screenshot.
//!
//! `cargo run -p agito --example screenshot -- https://example.com shot.png`

use agito::{ContextOptions, LaunchOptions, ScreenshotOptions};

#[tokio::main]
async fn main() -> agito::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| "https://example.com".into());
    let out = args.next().unwrap_or_else(|| "screenshot.png".into());

    let browser = agito::launch(LaunchOptions::default()).await?;
    println!("launched {}", browser.version());
    let page = browser.new_page(ContextOptions::default()).await?;
    let response = page.goto(&url).await?;
    if let Some(r) = &response {
        println!("{} {} -> {:?}", r.status, r.url, page.title().await?);
    }
    let png = page
        .screenshot(ScreenshotOptions {
            full_page: true,
            ..Default::default()
        })
        .await?;
    std::fs::write(&out, png)
        .map_err(|e| agito::Error::from(agito::ErrorKind::Other(e.to_string())))?;
    println!("saved {out}");
    browser.close().await
}

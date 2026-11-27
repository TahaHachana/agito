//! `agito screenshot` and `agito pdf`: open one page in a fresh headless browser and save it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agito::{
    Browser, ContextOptions, ImageFormat, LaunchOptions, NavigateOptions, Page, PdfOptions,
    ScreenshotOptions, Viewport, WaitUntil,
};
use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};

const BANNERS: &str = include_str!("../js/banners.js");

/// What every capture shares: which page, how to load it, and what to do before capturing.
#[derive(Args, Debug)]
pub struct PageArgs {
    /// The page: a URL, a local HTML file, or a host name (https is assumed).
    pub url: String,

    /// Seconds to wait for the page to load.
    #[arg(long, default_value_t = 60)]
    pub timeout: u64,

    /// When the page counts as loaded.
    #[arg(long, value_enum, default_value_t = Wait::Load)]
    pub wait_until: Wait,

    /// Seconds to wait after loading, for animations or late content.
    #[arg(long, default_value_t = 0.0)]
    pub delay: f64,

    /// Remove cookie-consent and privacy banners first.
    #[arg(long)]
    pub hide_banners: bool,
}

#[derive(Args, Debug)]
pub struct ScreenshotArgs {
    #[command(flatten)]
    pub page: PageArgs,

    /// Where to save it. The extension picks the format: .png, .jpg/.jpeg or .webp.
    #[arg(short, long, default_value = "screenshot.png")]
    pub output: PathBuf,

    /// Capture the whole scrollable page, not only the viewport.
    #[arg(long)]
    pub full_page: bool,

    /// Viewport width in CSS pixels.
    #[arg(long, default_value_t = 1280)]
    pub width: u32,

    /// Viewport height in CSS pixels.
    #[arg(long, default_value_t = 720)]
    pub height: u32,

    /// JPEG or WebP quality, 0-100.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub quality: Option<u8>,
}

#[derive(Args, Debug)]
pub struct PdfArgs {
    #[command(flatten)]
    pub page: PageArgs,

    /// Where to save it.
    #[arg(short, long, default_value = "output.pdf")]
    pub output: PathBuf,

    /// Viewport width in CSS pixels, which decides the page's responsive layout.
    #[arg(long, default_value_t = 1440)]
    pub width: u32,

    /// Print in landscape.
    #[arg(long)]
    pub landscape: bool,

    /// Print background colours and images, which browsers leave out of print by default.
    #[arg(long)]
    pub background: bool,
}

/// `--wait-until`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Wait {
    /// The load event.
    Load,
    /// DOMContentLoaded.
    #[value(name = "domcontentloaded")]
    DomContentLoaded,
    /// No network connections for 500ms.
    #[value(name = "networkidle")]
    NetworkIdle,
    /// The navigation committed; nothing more is awaited.
    Commit,
}

impl From<Wait> for WaitUntil {
    fn from(wait: Wait) -> Self {
        match wait {
            Wait::Load => WaitUntil::Load,
            Wait::DomContentLoaded => WaitUntil::DomContentLoaded,
            Wait::NetworkIdle => WaitUntil::NetworkIdle,
            Wait::Commit => WaitUntil::Commit,
        }
    }
}

pub async fn screenshot(args: ScreenshotArgs) -> Result<()> {
    let format = image_format(&args.output)?;
    if args.quality.is_some() && format == ImageFormat::Png {
        bail!("--quality applies to JPEG and WebP, not PNG");
    }
    let viewport = Viewport {
        width: args.width,
        height: args.height,
    };
    let bytes = with_page(&args.page, viewport, async |page| {
        Ok(page
            .screenshot(ScreenshotOptions {
                format,
                quality: args.quality,
                full_page: args.full_page,
                clip: None,
            })
            .await?)
    })
    .await?;
    save(&args.output, &bytes)?;
    println!("Screenshot saved to {}", args.output.display());
    Ok(())
}

pub async fn pdf(args: PdfArgs) -> Result<()> {
    // The height only matters to layouts sized by the viewport; print lays out the full page.
    let viewport = Viewport {
        width: args.width,
        height: 900,
    };
    let bytes = with_page(&args.page, viewport, async |page| {
        Ok(page
            .pdf(PdfOptions {
                landscape: args.landscape,
                print_background: args.background,
                ..Default::default()
            })
            .await?)
    })
    .await?;
    save(&args.output, &bytes)?;
    println!("PDF saved to {}", args.output.display());
    Ok(())
}

/// Launches a browser as configured (`agito.config.json`, `AGITO_*`, then the browser `agito
/// install` recorded), loads the page, runs `capture`, and closes the browser whatever happened.
async fn with_page<T>(
    args: &PageArgs,
    viewport: Viewport,
    capture: impl AsyncFnOnce(&Page) -> Result<T>,
) -> Result<T> {
    let url = target_url(&args.url)?;
    // Scrollbars would paint a strip down the right of every capture and narrow a full-page
    // one to the content width; hidden, the page lays out across the whole viewport.
    let mut options = LaunchOptions::default();
    options.args.push("--hide-scrollbars".into());
    let browser = agito::launch(options)
        .await
        .context("could not launch a browser (run `agito install` to download one)")?;
    let outcome = async {
        let page = browser
            .new_page(ContextOptions {
                viewport: Some(viewport),
                ..Default::default()
            })
            .await?;
        load(&page, &url, args).await?;
        capture(&page).await
    }
    .await;
    close(browser).await;
    outcome
}

async fn close(browser: Browser) {
    if let Err(err) = browser.close().await {
        eprintln!("warning: the browser did not close cleanly: {err}");
    }
}

async fn load(page: &Page, url: &str, args: &PageArgs) -> Result<()> {
    let response = page
        .main_frame()
        .goto_with(
            url,
            NavigateOptions {
                wait_until: args.wait_until.into(),
                timeout: Some(Duration::from_secs(args.timeout)),
                referer: None,
            },
        )
        .await
        .with_context(|| format!("could not load {url}"))?;
    // An error page is still a page, and capturing it is sometimes the point, so this warns.
    if let Some(response) = response.filter(|r| r.status() >= 400) {
        eprintln!(
            "warning: {url} answered {} {}",
            response.status(),
            response.status_text()
        );
    }
    if args.delay > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(args.delay)).await;
    }
    if args.hide_banners {
        let removed: u32 = page.evaluate(BANNERS).await?;
        if removed > 0 {
            eprintln!("Removed {removed} banner(s).");
        }
    }
    Ok(())
}

/// The URL to load for `target`: a URL as given, an existing file as a `file:` URL, and anything
/// else as a web address, so `agito pdf report.html` and `agito screenshot example.com` work.
/// A bare address is https, except on the loopback host, where development servers listen
/// without TLS.
pub fn target_url(target: &str) -> Result<String> {
    // `localhost:3000` and `C:\page.html` parse as URLs with schemes `localhost` and `c`, so a
    // parse alone is not enough: a URL must name a host or use a scheme that needs none.
    if let Ok(url) = url::Url::parse(target)
        && (url.has_host() || matches!(url.scheme(), "data" | "about" | "file" | "blob"))
    {
        return Ok(url.into());
    }
    let path = Path::new(target);
    if path.exists() {
        let absolute = std::path::absolute(path)
            .with_context(|| format!("could not resolve {}", path.display()))?;
        return url::Url::from_file_path(&absolute)
            .map(Into::into)
            .map_err(|()| anyhow::anyhow!("{} cannot be opened as a URL", absolute.display()));
    }
    let host = target.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    let scheme = if matches!(host, "localhost" | "127.0.0.1" | "[::1]") {
        "http"
    } else {
        "https"
    };
    Ok(format!("{scheme}://{target}"))
}

fn image_format(output: &Path) -> Result<ImageFormat> {
    let extension = output
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    Ok(match extension.as_deref() {
        Some("png") => ImageFormat::Png,
        Some("jpg" | "jpeg") => ImageFormat::Jpeg,
        Some("webp") => ImageFormat::Webp,
        _ => bail!(
            "cannot tell the image format of {}: use .png, .jpg, .jpeg or .webp",
            output.display()
        ),
    })
}

fn save(output: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    std::fs::write(output, bytes).with_context(|| format!("could not write {}", output.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_become_urls() {
        let url = |s: &str| target_url(s).unwrap();
        assert_eq!(url("https://example.com/a"), "https://example.com/a");
        assert_eq!(url("http://localhost:3000"), "http://localhost:3000/");
        assert_eq!(url("example.com"), "https://example.com");
        assert_eq!(url("example.com/path?q=1"), "https://example.com/path?q=1");
        assert_eq!(url("localhost:3000"), "http://localhost:3000");
        assert_eq!(url("127.0.0.1:8080/app"), "http://127.0.0.1:8080/app");
        assert_eq!(url("localhost"), "http://localhost");
        assert_eq!(
            url("localhost.example.com"),
            "https://localhost.example.com"
        );
        assert_eq!(url("data:text/html,<p>hi"), "data:text/html,<p>hi");
        assert_eq!(url("about:blank"), "about:blank");
        assert_eq!(url("file:///tmp/x.html"), "file:///tmp/x.html");

        let file = std::env::temp_dir().join(format!("agito-target-{}.html", std::process::id()));
        std::fs::write(&file, "<p>hi</p>").unwrap();
        let as_url = url(file.to_str().unwrap());
        assert!(as_url.starts_with("file:///"), "{as_url}");
        assert!(as_url.ends_with(".html"), "{as_url}");
        std::fs::remove_file(file).unwrap();
    }

    /// What the banner script leaves of a page with a vendor dialog, a floating banner, and
    /// look-alikes that must stay: a footer privacy link, a static cookie recipe, and a body
    /// whose class mentions cookies.
    #[cfg(feature = "integration")]
    #[tokio::test]
    async fn banner_removal_takes_only_what_floats() {
        let browser = agito::launch(LaunchOptions::default()).await.unwrap();
        let page = browser.new_page(ContextOptions::default()).await.unwrap();
        page.set_content(
            r#"<html style="overflow:hidden"><body class="cookie-free modal-open" style="overflow:hidden">
              <div id="onetrust-banner-sdk">vendor</div>
              <div class="wrapper" style="position:fixed;bottom:0">
                <div class="cookie-notice">We use cookies <a class="privacy-more">more</a></div>
              </div>
              <article class="cookie-recipe">Chocolate chip</article>
              <footer><a class="privacy-link" href="/privacy">Privacy</a></footer>
            </body></html>"#,
        )
        .await
        .unwrap();

        let removed: u32 = page.evaluate(BANNERS).await.unwrap();
        assert_eq!(removed, 2, "the vendor dialog and the fixed wrapper");
        let left: Vec<String> = page
            .evaluate(
                "[...document.body.querySelectorAll('[id],[class]')].map(e => e.id || e.className)",
            )
            .await
            .unwrap();
        assert_eq!(left, ["cookie-recipe", "privacy-link"]);
        let unlocked: bool = page
            .evaluate(
                "document.body.style.overflow === '' && document.documentElement.style.overflow === '' \
                 && !document.body.classList.contains('modal-open')",
            )
            .await
            .unwrap();
        assert!(unlocked);
        browser.close().await.unwrap();
    }

    #[test]
    fn the_extension_picks_the_image_format() {
        let format = |s: &str| image_format(Path::new(s));
        assert_eq!(format("a.png").unwrap(), ImageFormat::Png);
        assert_eq!(format("a.JPG").unwrap(), ImageFormat::Jpeg);
        assert_eq!(format("dir/a.jpeg").unwrap(), ImageFormat::Jpeg);
        assert_eq!(format("a.webp").unwrap(), ImageFormat::Webp);
        assert!(format("a.gif").is_err());
        assert!(format("screenshot").is_err());
    }
}

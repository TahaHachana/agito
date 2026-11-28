//! A [Model Context Protocol](https://modelcontextprotocol.io) server that lets AI agents drive
//! a real browser through agito: navigate, read a page as an accessibility snapshot, act on
//! its elements by ref or selector, and capture it.
//!
//! Run it with `agito mcp`. Agents perceive pages through the accessibility tree rather than
//! pixels, so they reason over a compact, labelled element list and address elements by the
//! refs a snapshot assigns.
#![warn(missing_docs)]

mod session;
mod snapshot;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agito::{BrowserChannel, ContextOptions, LaunchOptions, Viewport};
use rmcp::ServiceExt;

pub use tools::AgitoServer;

/// How `agito mcp` starts its browser and what it allows. Every field left as `None` falls
/// back to agito's configuration (`agito.config.json` and `AGITO_*` variables) and then to the
/// server's own defaults.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// Run the browser without a window. Default: true.
    pub headless: Option<bool>,
    /// Which browser to drive.
    pub channel: Option<BrowserChannel>,
    /// Start this browser binary instead of resolving one from the channel.
    pub executable_path: Option<PathBuf>,
    /// Extra command-line arguments for the browser.
    pub browser_args: Vec<String>,
    /// A browser profile directory, kept between runs.
    pub user_data_dir: Option<PathBuf>,
    /// The viewport of every page. Default: 1280x800.
    pub viewport: Option<(u32, u32)>,
    /// The user agent every page reports.
    pub user_agent: Option<String>,
    /// The locale every page formats with, such as `en-GB`.
    pub locale: Option<String>,
    /// The time zone every page reports, such as `Europe/Berlin`.
    pub timezone: Option<String>,
    /// How long an element action waits for its target.
    pub timeout: Option<Duration>,
    /// How long a navigation waits to finish.
    pub navigation_timeout: Option<Duration>,
    /// Let tools open `file://` URLs.
    pub allow_unrestricted_file_access: bool,
}

/// The viewport a session's pages get unless told otherwise.
pub const DEFAULT_VIEWPORT: (u32, u32) = (1280, 800);

impl ServerOptions {
    pub(crate) fn launch_options(&self) -> LaunchOptions {
        let defaults = LaunchOptions::default();
        LaunchOptions {
            headless: self.headless.unwrap_or(defaults.headless),
            channel: self.channel.or(defaults.channel),
            executable_path: self
                .executable_path
                .clone()
                .or(defaults.executable_path.clone()),
            args: self.browser_args.clone(),
            user_data_dir: self.user_data_dir.clone(),
            ..defaults
        }
    }

    pub(crate) fn context_options(&self) -> ContextOptions {
        let defaults = ContextOptions::default();
        let (width, height) = self.viewport.unwrap_or(DEFAULT_VIEWPORT);
        ContextOptions {
            viewport: Some(Viewport { width, height }),
            user_agent: self.user_agent.clone().or(defaults.user_agent.clone()),
            locale: self.locale.clone().or(defaults.locale.clone()),
            timezone_id: self.timezone.clone().or(defaults.timezone_id.clone()),
            action_timeout: self.timeout.unwrap_or(defaults.action_timeout),
            navigation_timeout: self
                .navigation_timeout
                .unwrap_or(defaults.navigation_timeout),
            // An agent's session is not a test: automatic audits would only slow it down.
            accessibility: agito::accessibility::AccessibilityOptions::off(),
            ..defaults
        }
    }
}

/// Serves MCP over standard input and output until the client disconnects, then closes the
/// browser the session started.
pub async fn serve_stdio(
    options: ServerOptions,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    serve(options, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Serves MCP over a pair of byte streams, newline-delimited JSON-RPC, until the client
/// disconnects, then closes the browser the session started.
pub async fn serve<R, W>(
    options: ServerOptions,
    reader: R,
    writer: W,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let server = AgitoServer::new(Arc::new(options));
    let session = server.session();
    let outcome = match server.serve((reader, writer)).await {
        Ok(service) => service.waiting().await.map(drop).map_err(Into::into),
        Err(e) => Err(e.into()),
    };
    session.lock().await.shutdown().await;
    outcome
}

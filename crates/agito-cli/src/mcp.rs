//! `agito mcp`: the Model Context Protocol server, over stdio.

use std::path::PathBuf;
use std::time::Duration;

use agito::BrowserChannel;
use agito_mcp::{Capability, DialogPolicy, HttpOptions, ServerOptions};
use anyhow::{Result, anyhow, bail};
use clap::Args;

/// Serve a Model Context Protocol server over stdio, so AI agents can drive a browser.
#[derive(Args)]
pub struct McpArgs {
    /// Run the browser without a window. `--headless false` shows it.
    #[arg(long, value_name = "BOOL", num_args = 0..=1, default_missing_value = "true")]
    headless: Option<bool>,

    /// The browser to drive: chromium, chrome or edge.
    #[arg(long)]
    channel: Option<BrowserChannel>,

    /// Start this browser binary instead of resolving one from --channel.
    #[arg(long, value_name = "PATH")]
    executable_path: Option<PathBuf>,

    /// An extra browser argument. Repeat for more: --browser-arg=--no-sandbox.
    #[arg(long = "browser-arg", value_name = "ARG", allow_hyphen_values = true)]
    browser_args: Vec<String>,

    /// A browser profile directory, so cookies and sign-ins persist between runs.
    #[arg(long, value_name = "DIR")]
    user_data_dir: Option<PathBuf>,

    /// The viewport of every page, as WIDTHxHEIGHT. Default: 1280x800.
    #[arg(long, value_name = "WxH", value_parser = parse_viewport)]
    viewport: Option<(u32, u32)>,

    /// The user agent every page reports.
    #[arg(long)]
    user_agent: Option<String>,

    /// The locale every page formats with, such as en-GB.
    #[arg(long)]
    locale: Option<String>,

    /// The time zone every page reports, such as Europe/Berlin.
    #[arg(long)]
    timezone: Option<String>,

    /// How long an element action waits for its target, in milliseconds.
    #[arg(long, value_name = "MS")]
    timeout: Option<u64>,

    /// How long a navigation waits to finish, in milliseconds.
    #[arg(long, value_name = "MS")]
    navigation_timeout: Option<u64>,

    /// How long an action waits for the page to show what it did, in milliseconds. 0 reports
    /// the instant the action returns. Default: 500.
    #[arg(long, value_name = "MS")]
    settle: Option<u64>,

    /// What becomes of a JavaScript dialog: ask leaves it for handle_dialog, accept or dismiss
    /// answers it at once.
    #[arg(long, value_name = "POLICY", default_value = "ask")]
    dialogs: DialogPolicy,

    /// Optional tool groups to add: coordinates, contexts, routing. Separate them with commas
    /// or repeat the flag. Outranks AGITO_MCP_CAPS.
    #[arg(long = "caps", value_name = "GROUPS")]
    caps: Vec<String>,

    /// Let tools open file:// URLs and read files from anywhere.
    #[arg(long)]
    allow_unrestricted_file_access: bool,

    /// Drive the browser already running at this debugging endpoint (http://127.0.0.1:9222 or
    /// its WebSocket URL) instead of starting one. It is never closed by the server.
    #[arg(long, value_name = "ENDPOINT")]
    connect: Option<String>,

    /// List browser_attach, so an agent can attach to a running browser mid-session. Implied
    /// by --connect.
    #[arg(long)]
    allow_attach: bool,

    /// Serve Streamable HTTP for remote clients instead of stdio. Each client session gets
    /// its own browser.
    #[arg(long)]
    http: bool,

    /// The interface to bind with --http. Any but loopback needs --token.
    #[arg(long, default_value = "127.0.0.1", requires = "http")]
    host: std::net::IpAddr,

    /// The port to listen on with --http.
    #[arg(long, default_value_t = 8931, requires = "http")]
    port: u16,

    /// The bearer token every HTTP request must carry. Also AGITO_MCP_TOKEN.
    #[arg(long, requires = "http")]
    token: Option<String>,
}

fn parse_viewport(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("'{s}' is not WIDTHxHEIGHT, such as 1280x800"))?;
    let parse = |v: &str| {
        v.trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| format!("'{s}' is not WIDTHxHEIGHT, such as 1280x800"))
    };
    Ok((parse(w)?, parse(h)?))
}

pub async fn mcp(args: McpArgs) -> Result<()> {
    if args.channel == Some(BrowserChannel::Firefox) {
        bail!("agito mcp drives Chromium-based browsers only: chromium, chrome or edge");
    }
    // The flag outranks the variable, which is how a client's configuration sets groups
    // without restating the command.
    let caps = if args.caps.is_empty() {
        std::env::var("AGITO_MCP_CAPS").unwrap_or_default()
    } else {
        args.caps.join(",")
    };
    let capabilities = Capability::parse_list(&caps).map_err(|e| anyhow!("--caps: {e}"))?;
    if args.connect.is_some() {
        // These describe starting a browser, or emulating one, and an attached session drives
        // the browser's own context as it is.
        let unused: Vec<&str> = [
            ("--headless", args.headless.is_some()),
            ("--channel", args.channel.is_some()),
            ("--executable-path", args.executable_path.is_some()),
            ("--browser-arg", !args.browser_args.is_empty()),
            ("--user-data-dir", args.user_data_dir.is_some()),
            ("--viewport", args.viewport.is_some()),
            ("--user-agent", args.user_agent.is_some()),
            ("--locale", args.locale.is_some()),
            ("--timezone", args.timezone.is_some()),
        ]
        .into_iter()
        .filter_map(|(flag, given)| given.then_some(flag))
        .collect();
        if !unused.is_empty() {
            eprintln!(
                "agito mcp: {} describe{} starting or emulating a browser, so with --connect {} \
                 no effect on the browser's own tabs",
                unused.join(", "),
                if unused.len() == 1 { "s" } else { "" },
                if unused.len() == 1 {
                    "it has"
                } else {
                    "they have"
                }
            );
        }
    }
    let options = ServerOptions {
        headless: args.headless,
        channel: args.channel,
        executable_path: args.executable_path,
        browser_args: args.browser_args,
        user_data_dir: args.user_data_dir,
        viewport: args.viewport,
        user_agent: args.user_agent,
        locale: args.locale,
        timezone: args.timezone,
        timeout: args.timeout.map(Duration::from_millis),
        navigation_timeout: args.navigation_timeout.map(Duration::from_millis),
        settle: args.settle.map(Duration::from_millis),
        dialogs: args.dialogs,
        capabilities,
        connect: args.connect,
        allow_attach: args.allow_attach,
        allow_unrestricted_file_access: args.allow_unrestricted_file_access,
    };
    if !args.http {
        return agito_mcp::serve_stdio(options)
            .await
            .map_err(|e| anyhow!("the MCP server stopped: {e}"));
    }
    let http = HttpOptions {
        host: args.host,
        port: args.port,
        token: args
            .token
            .or_else(|| std::env::var("AGITO_MCP_TOKEN").ok())
            .filter(|t| !t.is_empty()),
    };
    agito_mcp::check_http(&http).map_err(|e| anyhow!(e))?;
    let guarded = http.token.is_some();
    agito_mcp::serve_http(
        options,
        http,
        |address| {
            eprintln!(
                "agito mcp: serving Streamable HTTP at http://{address}/mcp{}",
                if guarded {
                    ", bearer token required"
                } else {
                    ""
                }
            );
        },
        async {
            let _ = tokio::signal::ctrl_c().await;
        },
    )
    .await
    .map_err(|e| anyhow!("the MCP server stopped: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewports_parse() {
        assert_eq!(parse_viewport("1280x800"), Ok((1280, 800)));
        assert_eq!(parse_viewport("640X480"), Ok((640, 480)));
        assert!(parse_viewport("1280").is_err());
        assert!(parse_viewport("0x10").is_err());
    }
}

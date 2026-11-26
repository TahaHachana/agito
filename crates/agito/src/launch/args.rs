//! Chromium command-line flags.

use std::path::Path;

use super::LaunchOptions;

/// Flags every launch gets unless listed in `ignore_default_args`.
pub const DEFAULT_ARGS: &[&str] = &[
    "--disable-background-networking",
    "--disable-background-timer-throttling",
    "--disable-backgrounding-occluded-windows",
    "--disable-client-side-phishing-detection",
    "--disable-default-apps",
    "--disable-extensions",
    "--disable-hang-monitor",
    "--disable-popup-blocking",
    "--disable-prompt-on-repost",
    "--disable-renderer-backgrounding",
    "--disable-sync",
    "--disable-translate",
    "--metrics-recording-only",
    "--no-first-run",
    "--safebrowsing-disable-auto-update",
];

/// Builds the argument list. `debugging_port` selects WebSocket mode; `None` selects the pipe.
pub fn chromium_args(
    options: &LaunchOptions,
    user_data_dir: &Path,
    debugging_port: Option<u16>,
) -> Vec<String> {
    let mut args: Vec<String> = DEFAULT_ARGS
        .iter()
        .filter(|a| !options.ignore_default_args.iter().any(|i| i == *a))
        .map(|a| a.to_string())
        .collect();
    args.push(match debugging_port {
        Some(port) => format!("--remote-debugging-port={port}"),
        None => "--remote-debugging-pipe".to_owned(),
    });
    args.push(format!("--user-data-dir={}", user_data_dir.display()));
    if options.headless {
        args.push("--headless=new".to_owned());
    } else {
        args.push("--no-startup-window".to_owned());
        args.push("--disable-blink-features=AutomationControlled".to_owned());
    }
    if let Some(dir) = &options.downloads_path {
        args.push(format!("--download-default-directory={}", dir.display()));
    }
    args.extend(options.args.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_pipe_launch() {
        let options = LaunchOptions {
            headless: true,
            ..LaunchOptions::default()
        };
        let args = chromium_args(&options, Path::new("/tmp/p"), None);
        assert!(args.contains(&"--remote-debugging-pipe".to_owned()));
        assert!(args.contains(&"--headless=new".to_owned()));
        assert!(args.contains(&"--user-data-dir=/tmp/p".to_owned()));
        assert!(args.contains(&"--no-first-run".to_owned()));
    }

    #[test]
    fn headed_port_launch_with_overrides() {
        let options = LaunchOptions {
            headless: false,
            args: vec!["--foo".into()],
            ignore_default_args: vec!["--no-first-run".into()],
            ..LaunchOptions::default()
        };
        let args = chromium_args(&options, Path::new("/tmp/p"), Some(9222));
        assert!(args.contains(&"--remote-debugging-port=9222".to_owned()));
        assert!(args.contains(&"--no-startup-window".to_owned()));
        assert!(!args.contains(&"--no-first-run".to_owned()));
        assert_eq!(args.last().unwrap(), "--foo");
    }
}

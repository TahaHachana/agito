//! `agito.config.json` and `AGITO_*` environment variables.
//!
//! The file is found by searching upward from the working directory. Environment variables
//! override the file; explicit options passed in code override both.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

/// The whole configuration file. Every field is optional.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    /// `launch`: how to start the browser.
    pub launch: LaunchConfig,
    /// `context`: the default context options.
    pub context: ContextConfig,
    /// `locator`: action settings.
    pub locator: LocatorConfig,
    /// `assertions`: `expect` settings.
    pub assertions: AssertionsConfig,
    /// `failure`: what to save when a test fails.
    pub failure: FailureConfig,
    /// `accessibility`: automatic accessibility audits.
    pub accessibility: AccessibilityConfig,
    /// `performance`: the default performance budget.
    pub performance: PerformanceConfig,
}

/// The `launch` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LaunchConfig {
    /// Run without a window.
    pub headless: Option<bool>,
    /// `chrome`, `edge` or `chromium`.
    pub channel: Option<String>,
    /// A pause before every command, in milliseconds.
    pub slow_mo: Option<u64>,
    /// How long launching may take, in milliseconds.
    pub timeout: Option<u64>,
    /// A browser executable to launch.
    pub executable_path: Option<String>,
    /// Run Chromium with its sandbox.
    pub chromium_sandbox: Option<bool>,
    /// Log every protocol message to this file, or to `stderr`.
    pub protocol_log: Option<String>,
}

/// The `context` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContextConfig {
    /// The locale, such as `fr-FR`.
    pub locale: Option<String>,
    /// `light`, `dark` or `no-preference`.
    pub color_scheme: Option<String>,
    /// Accept invalid TLS certificates.
    #[serde(rename = "ignoreHTTPSErrors")]
    pub ignore_https_errors: Option<bool>,
    /// The viewport size.
    pub viewport: Option<ViewportConfig>,
    /// What relative URLs resolve against.
    #[serde(rename = "baseURL")]
    pub base_url: Option<String>,
}

/// A viewport size in CSS pixels.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewportConfig {
    /// Width.
    pub width: Option<u32>,
    /// Height.
    pub height: Option<u32>,
}

/// The `locator` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LocatorConfig {
    /// Default action timeout, in milliseconds.
    pub timeout: Option<u64>,
}

/// The `assertions` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AssertionsConfig {
    /// Default assertion timeout, in milliseconds.
    pub timeout: Option<u64>,
}

/// The `performance` section: the budget [`PerformanceBudget::default`] reads.
///
/// [`PerformanceBudget::default`]: crate::performance::PerformanceBudget
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PerformanceConfig {
    /// Largest Contentful Paint, in milliseconds.
    pub lcp: Option<f64>,
    /// First Contentful Paint, in milliseconds.
    pub fcp: Option<f64>,
    /// Time To First Byte, in milliseconds.
    pub ttfb: Option<f64>,
    /// Cumulative Layout Shift.
    pub cls: Option<f64>,
    /// Interaction to Next Paint, in milliseconds.
    pub inp: Option<f64>,
    /// The JavaScript heap in use, in bytes.
    pub js_heap_size: Option<u64>,
    /// How many DOM nodes a page holds.
    pub dom_nodes: Option<u64>,
}

/// The `accessibility` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AccessibilityConfig {
    /// `off`, `warn` or `enforce`.
    pub mode: Option<String>,
    /// Audit after each navigation.
    pub audit_after_navigation: Option<bool>,
    /// Also audit after actions that change a page.
    pub audit_after_actions: Option<bool>,
    /// Whether warnings fail a test in enforce mode.
    pub include_warnings: Option<bool>,
    /// Rule ids not to run.
    pub skip_rules: Option<Vec<String>>,
}

/// The `failure` section.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FailureConfig {
    /// Save a screenshot when an `#[agito::test]` fails.
    pub screenshot: Option<bool>,
    /// Where to save them.
    pub screenshot_path: Option<String>,
    /// Record a trace of each `#[agito::test]`, and keep it when the test fails.
    pub trace: Option<bool>,
    /// Where to save the traces.
    pub trace_path: Option<String>,
}

/// The name of the configuration file.
pub const FILE_NAME: &str = "agito.config.json";

static CONFIG: OnceLock<Config> = OnceLock::new();

/// The configuration for this process, loaded once from the file and the environment.
pub fn get() -> &'static Config {
    CONFIG.get_or_init(|| {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        load(&cwd, &|name| std::env::var(name).ok())
    })
}

/// Loads the configuration starting the file search at `start`, reading variables through
/// `env`. A file that cannot be parsed is reported on stderr and ignored.
pub fn load(start: &Path, env: &dyn Fn(&str) -> Option<String>) -> Config {
    let mut config = find_file(start)
        .and_then(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            match serde_json::from_str::<Config>(&text) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("agito: ignoring {}: {e}", path.display());
                    None
                }
            }
        })
        .unwrap_or_default();
    apply_env(&mut config, env);
    config
}

fn find_file(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

fn parse_bool(value: Option<String>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_u64(value: Option<String>) -> Option<u64> {
    value?.trim().parse().ok()
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

fn apply_env(c: &mut Config, env: &dyn Fn(&str) -> Option<String>) {
    if let Some(v) = parse_bool(env("AGITO_HEADLESS")) {
        c.launch.headless = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_CHANNEL")) {
        c.launch.channel = Some(v);
    }
    if let Some(v) = parse_u64(env("AGITO_SLOWMO")) {
        c.launch.slow_mo = Some(v);
    }
    if let Some(v) = parse_u64(env("AGITO_LAUNCH_TIMEOUT")) {
        c.launch.timeout = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_EXECUTABLE_PATH")) {
        c.launch.executable_path = Some(v);
    }
    if let Some(v) = non_empty(env(crate::transport::log::ENV_VAR)) {
        c.launch.protocol_log = Some(v);
    }
    if let Some(v) = parse_bool(env("AGITO_CHROMIUM_SANDBOX")) {
        c.launch.chromium_sandbox = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_LOCALE")) {
        c.context.locale = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_COLOR_SCHEME")) {
        c.context.color_scheme = Some(v);
    }
    if let Some(v) = parse_bool(env("AGITO_IGNORE_HTTPS_ERRORS")) {
        c.context.ignore_https_errors = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_BASE_URL")) {
        c.context.base_url = Some(v);
    }
    if let Some(v) = parse_u64(env("AGITO_LOCATOR_TIMEOUT")) {
        c.locator.timeout = Some(v);
    }
    if let Some(v) = parse_u64(env("AGITO_ASSERTIONS_TIMEOUT")) {
        c.assertions.timeout = Some(v);
    }
    if let Some(v) = parse_bool(env("AGITO_FAILURES_SCREENSHOT")) {
        c.failure.screenshot = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_FAILURES_SCREENSHOT_PATH")) {
        c.failure.screenshot_path = Some(v);
    }
    if let Some(v) = parse_bool(env("AGITO_FAILURES_TRACE")) {
        c.failure.trace = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_FAILURES_TRACE_PATH")) {
        c.failure.trace_path = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_ACCESSIBILITY_MODE")) {
        c.accessibility.mode = Some(v);
    }
    if let Some(v) = non_empty(env("AGITO_ACCESSIBILITY_SKIP_RULES")) {
        c.accessibility.skip_rules = Some(
            v.split(',')
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned)
                .collect(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn file_is_found_upward_and_env_overrides() {
        let root = std::env::temp_dir().join(format!("agito-config-{}", std::process::id()));
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            root.join(FILE_NAME),
            r#"{ "launch": { "headless": false, "slowMo": 5 }, "assertions": { "timeout": 2000 },
                 "context": { "viewport": { "width": 800, "height": 600 }, "baseURL": "http://x" } }"#,
        )
        .unwrap();
        let c = load(
            &nested,
            &env(&[("AGITO_SLOWMO", "7"), ("AGITO_HEADLESS", "garbage")]),
        );
        assert_eq!(c.launch.headless, Some(false));
        assert_eq!(c.launch.slow_mo, Some(7));
        assert_eq!(c.assertions.timeout, Some(2000));
        assert_eq!(c.context.viewport.unwrap().width, Some(800));
        assert_eq!(c.context.base_url.as_deref(), Some("http://x"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_file_gives_defaults() {
        let c = load(
            Path::new("/nonexistent/agito"),
            &env(&[("AGITO_HEADLESS", "0")]),
        );
        assert_eq!(c.launch.headless, Some(false));
        assert_eq!(c.locator.timeout, None);
    }
}

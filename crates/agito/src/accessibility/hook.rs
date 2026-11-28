//! Auditing automatically: after navigations, and optionally after actions.

use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::audit::AuditOptions;
use super::rules::{Severity, Violation};
use crate::config;
use crate::error::Result;
use crate::page::{Page, Response};
use crate::plugin::{ActionResult, LifecycleHook};

/// What a context's automatic audits do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccessibilityMode {
    /// No automatic audits. The default.
    #[default]
    Off,
    /// Audit and report, but never fail a test.
    Warn,
    /// Audit, and fail an `#[agito::test]` that leaves errors (and, unless
    /// `include_warnings` is off, warnings) behind.
    Enforce,
}

impl FromStr for AccessibilityMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "" => Ok(AccessibilityMode::Off),
            "warn" => Ok(AccessibilityMode::Warn),
            "enforce" => Ok(AccessibilityMode::Enforce),
            other => Err(format!(
                "unknown accessibility mode '{other}': expected off, warn or enforce"
            )),
        }
    }
}

/// When a context audits pages for accessibility on its own, and what fails a test.
///
/// [`Default`] reads the `accessibility` section of `agito.config.json` and
/// `AGITO_ACCESSIBILITY_MODE`; with neither, audits are off.
#[derive(Debug, Clone)]
pub struct AccessibilityOptions {
    /// Off, warn or enforce.
    pub mode: AccessibilityMode,
    /// Audit after each navigation. Default: true.
    pub audit_after_navigation: bool,
    /// Also audit after clicks, fills, selects and checks, which change a page without
    /// navigating. Default: false, since it audits far more often.
    pub audit_after_actions: bool,
    /// Whether warnings fail a test in enforce mode, as errors do. Default: true.
    pub include_warnings: bool,
    /// Rule ids not to run.
    pub skip_rules: Vec<String>,
}

impl Default for AccessibilityOptions {
    fn default() -> Self {
        let c = &config::get().accessibility;
        let mode = c
            .mode
            .as_deref()
            .and_then(|m| match m.parse() {
                Ok(mode) => Some(mode),
                Err(e) => {
                    eprintln!("agito: {e}; accessibility audits are off");
                    None
                }
            })
            .unwrap_or_default();
        AccessibilityOptions {
            mode,
            audit_after_navigation: c.audit_after_navigation.unwrap_or(true),
            audit_after_actions: c.audit_after_actions.unwrap_or(false),
            include_warnings: c.include_warnings.unwrap_or(true),
            skip_rules: c.skip_rules.clone().unwrap_or_default(),
        }
    }
}

impl AccessibilityOptions {
    /// Automatic audits off, whatever the configuration says.
    pub fn off() -> Self {
        AccessibilityOptions {
            mode: AccessibilityMode::Off,
            ..Default::default()
        }
    }

    /// The violations among `found` that fail a test in enforce mode.
    pub fn failures<'a>(&self, found: &'a [Violation]) -> Vec<&'a Violation> {
        found
            .iter()
            .filter(|v| {
                v.severity == Severity::Error
                    || (v.severity == Severity::Warning && self.include_warnings)
            })
            .collect()
    }
}

/// The actions after which `audit_after_actions` audits: the ones that change a page.
const AUDITED_ACTIONS: &[&str] = &[
    "click",
    "dblclick",
    "fill",
    "selectOption",
    "check",
    "uncheck",
];

/// The lifecycle hook a context with automatic audits gets. It keeps what it finds, each
/// violation once, for the test harness and for `BrowserContext::accessibility_violations`.
pub(crate) struct AuditHook {
    options: AccessibilityOptions,
    found: Arc<Mutex<Vec<Violation>>>,
}

impl AuditHook {
    pub(crate) fn new(options: AccessibilityOptions, found: Arc<Mutex<Vec<Violation>>>) -> Self {
        AuditHook { options, found }
    }

    async fn audit(&self, page: &Page) {
        let options = AuditOptions {
            skip_rules: self.options.skip_rules.clone(),
            include_warnings: self.options.include_warnings,
        };
        // An audit that cannot run, on a page closing under it, say, reports nothing rather
        // than failing the step it follows.
        let Ok(result) = page.audit_accessibility(&options).await else {
            return;
        };
        let mut found = self.found.lock().unwrap();
        for violation in result.violations {
            let seen = found.iter().any(|v| {
                v.rule_id == violation.rule_id
                    && v.selector == violation.selector
                    && v.frame_url == violation.frame_url
                    && v.message == violation.message
            });
            if !seen {
                found.push(violation);
            }
        }
    }
}

#[async_trait]
impl LifecycleHook for AuditHook {
    async fn after_navigation(&self, page: &Page, _response: Option<&Response>) -> Result<()> {
        if self.options.audit_after_navigation {
            self.audit(page).await;
        }
        Ok(())
    }

    async fn after_action(&self, page: &Page, result: &ActionResult) -> Result<()> {
        if self.options.audit_after_actions
            && result.error.is_none()
            && AUDITED_ACTIONS.contains(&result.action.as_str())
        {
            self.audit(page).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_parse() {
        assert_eq!("Enforce".parse(), Ok(AccessibilityMode::Enforce));
        assert_eq!("warn".parse(), Ok(AccessibilityMode::Warn));
        assert_eq!("".parse(), Ok(AccessibilityMode::Off));
        assert!("strict".parse::<AccessibilityMode>().is_err());
    }

    #[test]
    fn failures_follow_include_warnings() {
        let found = vec![
            Violation::new("e", Severity::Error, "x"),
            Violation::new("w", Severity::Warning, "x"),
            Violation::new("i", Severity::Info, "x"),
        ];
        let mut options = AccessibilityOptions::off();
        options.include_warnings = true;
        assert_eq!(options.failures(&found).len(), 2);
        options.include_warnings = false;
        assert_eq!(options.failures(&found).len(), 1);
    }
}

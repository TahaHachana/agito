//! What the active transport can do, and guards that fail clearly when it cannot.

use crate::error::{Error, Result};

bitflags::bitflags! {
    /// Protocol features a transport natively supports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Capabilities: u32 {
        /// CDP Target-domain session multiplexing.
        const TARGET_MULTIPLEXING = 1 << 0;
        /// CDP Fetch-domain request interception.
        const FETCH_INTERCEPTION = 1 << 1;
        /// CDP Emulation-domain overrides.
        const EMULATION_OVERRIDES = 1 << 2;
        /// CDP Tracing domain.
        const TRACING = 1 << 3;
        /// WebDriver BiDi network interception.
        const BIDI_NETWORK_INTERCEPT = 1 << 4;
        /// WebDriver BiDi script evaluation.
        const BIDI_SCRIPT_EVALUATION = 1 << 5;
        /// WebDriver BiDi input actions.
        const BIDI_INPUT_ACTIONS = 1 << 6;
        /// CDP Security-domain overrides (certificate errors).
        const SECURITY_OVERRIDES = 1 << 7;
        /// CDP Accessibility-domain tree queries.
        const ACCESSIBILITY_TREE = 1 << 8;
        /// CDP Performance-domain metrics.
        const PERFORMANCE_METRICS = 1 << 9;
        /// CDP Profiler and CSS rule-usage coverage.
        const CODE_COVERAGE = 1 << 10;

        /// Everything a CDP transport supports.
        const ALL_CDP = Self::TARGET_MULTIPLEXING.bits() | Self::FETCH_INTERCEPTION.bits()
            | Self::EMULATION_OVERRIDES.bits() | Self::TRACING.bits()
            | Self::SECURITY_OVERRIDES.bits() | Self::ACCESSIBILITY_TREE.bits()
            | Self::PERFORMANCE_METRICS.bits() | Self::CODE_COVERAGE.bits();
        /// Everything a BiDi transport supports.
        const ALL_BIDI = Self::BIDI_NETWORK_INTERCEPT.bits() | Self::BIDI_SCRIPT_EVALUATION.bits()
            | Self::BIDI_INPUT_ACTIONS.bits();
    }
}

/// Fails with [`Error::NotSupported`] unless `has` includes all of `required`.
pub fn require(
    has: Capabilities,
    required: Capabilities,
    feature: &str,
    transport: Option<&str>,
) -> Result<()> {
    if has.contains(required) {
        return Ok(());
    }
    Err(Error::not_supported(match transport {
        Some(t) => format!("'{feature}' is not supported by the current browser transport ({t})."),
        None => format!("The active transport does not support '{feature}'."),
    }))
}

/// Fails unless the transport can read the accessibility tree, naming the feature that needed
/// it and, optionally, what to use instead.
pub fn require_accessibility_tree(
    has: Capabilities,
    transport: &str,
    feature: &str,
    alternative: Option<&str>,
) -> Result<()> {
    if has.contains(Capabilities::ACCESSIBILITY_TREE) {
        return Ok(());
    }
    let mut message = format!(
        "{feature} read the browser's accessibility tree, which only Chromium-based browsers expose, \
         and this browser is driven over {transport}."
    );
    if let Some(alt) = alternative {
        message.push(' ');
        message.push_str(alt);
    }
    Err(Error::not_supported(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_passes_and_fails() {
        assert!(
            require(
                Capabilities::ALL_CDP,
                Capabilities::TRACING,
                "tracing",
                None
            )
            .is_ok()
        );
        let err = require(
            Capabilities::ALL_BIDI,
            Capabilities::TRACING,
            "tracing",
            Some("Firefox/WebDriver BiDi"),
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "'tracing' is not supported by the current browser transport (Firefox/WebDriver BiDi)."
        );
    }

    #[test]
    fn accessibility_guard_mentions_alternative() {
        let err = require_accessibility_tree(
            Capabilities::ALL_BIDI,
            "Firefox/WebDriver BiDi",
            "Accessibility audits",
            Some("Use Chromium."),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .ends_with("Firefox/WebDriver BiDi. Use Chromium.")
        );
    }
}

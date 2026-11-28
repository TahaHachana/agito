//! Accessibility: the browser's accessibility tree, and audits of a page against rules such as
//! WCAG's.
//!
//! [`Page::accessibility_snapshot`](crate::Page::accessibility_snapshot) reads the tree, and
//! [`Page::audit_accessibility`](crate::Page::audit_accessibility) checks the page, and its
//! iframes, against every registered [`AccessibilityRule`]. `expect(&page)`'s
//! `to_pass_accessibility_audit` fails a test on what an audit finds.

pub mod audit;
pub mod contrast;
pub mod hook;
pub mod rules;
pub mod tree;

pub use audit::{AuditOptions, AuditResult};
pub use hook::{AccessibilityMode, AccessibilityOptions};
pub use rules::{AccessibilityRule, AuditedDocument, Severity, TextSample, Violation};
pub use tree::{AccessibilitySnapshot, AxNode};

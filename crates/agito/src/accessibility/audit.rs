//! Running an audit: collecting each document's tree and facts, running the rules, and placing
//! what they find.

use std::time::{Duration, Instant};

use agito_protocol::cdp::accessibility::GetFullAXTreeParams;
use agito_protocol::cdp::dom::ResolveNodeParams;
use serde::Deserialize;

use super::rules::{AuditedDocument, Severity, TextSample, Violation};
use super::tree::{self, AccessibilitySnapshot};
use crate::error::Result;
use crate::frame::Frame;
use crate::handles::call_function;
use crate::page::Page;
use crate::transport::capabilities;

const CSS_PATH: &str = include_str!("../../js/css_path.js");
const FACTS: &str = include_str!("../../js/accessibility_facts.js");

/// What an audit checks, and what fails `to_pass_accessibility_audit`.
#[derive(Debug, Clone)]
pub struct AuditOptions {
    /// Rule ids not to run, such as `a11y-color-contrast`.
    pub skip_rules: Vec<String>,
    /// Whether warnings fail `to_pass_accessibility_audit`, as errors do. Default: true.
    /// Audits report warnings either way.
    pub include_warnings: bool,
}

impl Default for AuditOptions {
    fn default() -> Self {
        AuditOptions {
            skip_rules: Vec::new(),
            include_warnings: true,
        }
    }
}

impl AuditOptions {
    /// Default options without the rules named.
    pub fn skipping(rules: &[&str]) -> AuditOptions {
        AuditOptions {
            skip_rules: rules.iter().map(|r| (*r).to_owned()).collect(),
            ..Default::default()
        }
    }
}

/// What an audit found.
#[derive(Debug, Clone, Default)]
pub struct AuditResult {
    /// Every violation, in the order documents and rules were checked.
    pub violations: Vec<Violation>,
    /// The ids of the rules that ran.
    pub rules: Vec<String>,
    /// How many documents were audited: the page's, and one per iframe.
    pub documents: usize,
    /// How many accessibility nodes were checked.
    pub nodes: usize,
    /// Frames that could not be audited, with why, such as one that went away mid-audit.
    pub skipped_frames: Vec<String>,
    /// How long the audit took.
    pub duration: Duration,
}

impl AuditResult {
    /// The violations that fail an audit under `options`: errors, and warnings unless
    /// `include_warnings` is off.
    pub fn failures(&self, options: &AuditOptions) -> Vec<&Violation> {
        self.violations
            .iter()
            .filter(|v| {
                v.severity == Severity::Error
                    || (v.severity == Severity::Warning && options.include_warnings)
            })
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Facts {
    language: Option<String>,
    duplicate_ids: Vec<String>,
    text: Vec<TextSample>,
}

impl Frame {
    /// This frame's document as assistive technology sees it.
    pub async fn accessibility_snapshot(&self) -> Result<AccessibilitySnapshot> {
        let session = self.session();
        capabilities::require_accessibility_tree(
            session.capabilities(),
            session.transport_description(),
            "accessibility snapshots and audits",
            None,
        )?;
        // Named by frame: a session asked for its own document answers with that document
        // alone, and the page's session does not describe a frame running in its own process.
        let result = session
            .send(&GetFullAXTreeParams {
                frame_id: Some(self.id().to_owned()),
                ..Default::default()
            })
            .await?;
        Ok(tree::build(result.nodes))
    }

    async fn audited_document(&self, is_top: bool) -> Result<AuditedDocument> {
        let tree = self.accessibility_snapshot().await?;
        // In a function, so cssPath is a declaration the collector can call.
        // The parentheses keep `return` from ending at the collector's leading comment.
        let script = format!("(() => {{\n{CSS_PATH}\nreturn (\n{FACTS}\n);\n}})()");
        let facts: Facts = self.evaluate(&script).await?;
        Ok(AuditedDocument {
            url: self.url(),
            is_top,
            tree,
            language: facts.language,
            duplicate_ids: facts.duplicate_ids,
            text: facts.text,
        })
    }

    /// A CSS selector for the element behind an accessibility node, if it can still be found.
    async fn selector_for(&self, backend_node_id: i64) -> Option<String> {
        let session = self.session();
        let resolved = session
            .send(&ResolveNodeParams {
                backend_node_id: Some(backend_node_id),
                ..Default::default()
            })
            .await
            .ok()?;
        let object_id = resolved.object.object_id?;
        let function = format!(
            "function() {{ {CSS_PATH} return this.nodeType === 1 ? cssPath(this) : null; }}"
        );
        let value = call_function(&session, &object_id, &function, &[], true, false)
            .await
            .ok()?
            .value?;
        value.as_str().map(str::to_owned)
    }
}

impl Page {
    /// The page's document as assistive technology sees it: its accessibility tree, without
    /// the nodes the browser ignores. Iframes appear as leaves; read theirs with
    /// [`Frame::accessibility_snapshot`].
    pub async fn accessibility_snapshot(&self) -> Result<AccessibilitySnapshot> {
        self.main_frame().accessibility_snapshot().await
    }

    /// Checks the page and its iframes against every registered accessibility rule (the
    /// built-in WCAG checks and any a plugin adds) and returns what they find.
    ///
    /// ```no_run
    /// # async fn example(page: agito::Page) -> agito::Result<()> {
    /// use agito::accessibility::AuditOptions;
    ///
    /// let result = page.audit_accessibility(&AuditOptions::default()).await?;
    /// for violation in &result.violations {
    ///     println!("{violation}");
    /// }
    /// # Ok(()) }
    /// ```
    pub async fn audit_accessibility(&self, options: &AuditOptions) -> Result<AuditResult> {
        let started = Instant::now();
        let rules: Vec<_> = self
            .context()
            .inner
            .accessibility_rules
            .iter()
            .filter(|r| !options.skip_rules.iter().any(|s| s == r.id()))
            .cloned()
            .collect();
        let mut result = AuditResult {
            rules: rules.iter().map(|r| r.id().to_owned()).collect(),
            ..Default::default()
        };
        let top = self.main_frame();
        for frame in self.frames() {
            let is_top = frame == top;
            let document = match frame.audited_document(is_top).await {
                Ok(document) => document,
                // The page's own document must be auditable; an iframe that went away or
                // refused is noted and the rest of the page still audited.
                Err(e) if is_top => return Err(e),
                Err(e) => {
                    result.skipped_frames.push(format!("{}: {e}", frame.url()));
                    continue;
                }
            };
            result.documents += 1;
            result.nodes += document.nodes().count();
            for rule in &rules {
                for mut violation in rule.check(&document) {
                    violation.frame_url.clone_from(&document.url);
                    if violation.selector.is_none()
                        && let Some(id) = violation.backend_node_id
                    {
                        violation.selector = frame.selector_for(id).await;
                    }
                    result.violations.push(violation);
                }
            }
        }
        result.duration = started.elapsed();
        Ok(result)
    }
}

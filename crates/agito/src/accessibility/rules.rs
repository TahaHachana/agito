//! Accessibility rules: the trait, what a rule can see, and the nine built-in rules.

use serde::{Deserialize, Serialize};

use super::contrast::{self, Rgba};
use super::tree::{AccessibilitySnapshot, AxNode};

/// How bad a violation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A definite failure that stops some people using the page.
    Error,
    /// A likely problem for some people.
    Warning,
    /// Worth knowing; not necessarily a defect.
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        })
    }
}

/// One thing a rule found wrong.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Violation {
    /// The rule that found it, such as `a11y-empty-button`.
    pub rule_id: String,
    /// How bad it is.
    pub severity: Severity,
    /// What is wrong and how to fix it.
    pub message: String,
    /// The role of the element at fault, when there is one.
    pub role: Option<String>,
    /// Its accessible name.
    pub name: Option<String>,
    /// A CSS selector for it, when one could be made.
    pub selector: Option<String>,
    /// The URL of the document it is in, which may be an iframe's.
    pub frame_url: String,
    /// The DOM node, for finding the element again.
    #[serde(skip)]
    pub backend_node_id: Option<i64>,
}

impl Violation {
    /// A violation of `rule_id`, not tied to an element yet.
    pub fn new(rule_id: &str, severity: Severity, message: impl Into<String>) -> Violation {
        Violation {
            rule_id: rule_id.to_owned(),
            severity,
            message: message.into(),
            role: None,
            name: None,
            selector: None,
            frame_url: String::new(),
            backend_node_id: None,
        }
    }

    /// Ties the violation to an accessibility node's element.
    pub fn at_node(mut self, node: &AxNode) -> Violation {
        self.role = Some(node.role.clone());
        self.name = (!node.name.is_empty()).then(|| node.name.clone());
        self.backend_node_id = node.backend_node_id;
        self
    }

    /// Ties the violation to the element a CSS selector finds.
    pub fn at_selector(mut self, selector: impl Into<String>) -> Violation {
        self.selector = Some(selector.into());
        self
    }
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.rule_id, self.message)?;
        if let Some(selector) = &self.selector {
            write!(f, " ({selector})")?;
        } else if let Some(role) = &self.role {
            write!(f, " ({role})")?;
        }
        Ok(())
    }
}

/// A run of visible text, as the page renders it, for contrast checks.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSample {
    /// A CSS selector for the element holding the text.
    pub selector: String,
    /// The start of the text.
    pub text: String,
    /// The computed `color`.
    pub color: String,
    /// The computed `background-color` of the element and its ancestors, nearest first, up to
    /// the first opaque one.
    pub backgrounds: Vec<String>,
    /// A background image or gradient lies behind the text, so its colour cannot be known.
    pub background_image: bool,
    /// The font size in CSS pixels.
    pub font_size: f64,
    /// The numeric font weight, such as 400 or 700.
    pub font_weight: f64,
}

/// One audited document, a page's or an iframe's: everything a rule can look at.
#[derive(Debug, Clone, Default)]
pub struct AuditedDocument {
    /// The document's URL.
    pub url: String,
    /// The page's own document, not an iframe's. Page-wide rules, such as needing a `main`
    /// landmark, only apply to it.
    pub is_top: bool,
    /// Its accessibility tree.
    pub tree: AccessibilitySnapshot,
    /// The `<html>` element's `lang`, if set.
    pub language: Option<String>,
    /// `id` values used by more than one element.
    pub duplicate_ids: Vec<String>,
    /// Visible text with its colours and size.
    pub text: Vec<TextSample>,
}

impl AuditedDocument {
    /// Every accessibility node, depth first.
    pub fn nodes(&self) -> impl Iterator<Item = &AxNode> {
        self.tree.nodes()
    }
}

/// An accessibility rule. Register one through
/// [`PluginContext::register_accessibility_rule`](crate::plugin::PluginContext::register_accessibility_rule)
/// and every audit runs it.
pub trait AccessibilityRule: Send + Sync + 'static {
    /// A stable id, such as `a11y-alt-text`, which audits can skip by.
    fn id(&self) -> &str;
    /// What the rule checks.
    fn description(&self) -> &str;
    /// The violations in `document`.
    fn check(&self, document: &AuditedDocument) -> Vec<Violation>;
}

fn is_hidden(node: &AxNode) -> bool {
    node.properties.get("hidden").is_some_and(|v| v == "true")
}

fn unnamed(node: &AxNode) -> bool {
    node.name.trim().is_empty() && !is_hidden(node)
}

/// Flags nodes with one of `roles` and no accessible name.
fn check_named(
    document: &AuditedDocument,
    rule: &str,
    roles: &[&str],
    message: impl Fn(&AxNode) -> String,
) -> Vec<Violation> {
    document
        .nodes()
        .filter(|n| roles.contains(&n.role.as_str()) && unnamed(n))
        .map(|n| Violation::new(rule, Severity::Error, message(n)).at_node(n))
        .collect()
}

/// Images need a name: alt text or `aria-label`.
pub struct AltText;
impl AccessibilityRule for AltText {
    fn id(&self) -> &str {
        "a11y-alt-text"
    }
    fn description(&self) -> &str {
        "Images must have a non-empty accessible name (alt text or aria-label)."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        check_named(document, self.id(), &["img", "image"], |_| {
            "Image has no accessible name. Add alt text, or aria-label; use alt=\"\" only for a \
             decorative image."
                .into()
        })
    }
}

/// Form controls need a label.
pub struct UnlabeledFormControl;
impl AccessibilityRule for UnlabeledFormControl {
    fn id(&self) -> &str {
        "a11y-unlabeled-form-control"
    }
    fn description(&self) -> &str {
        "Form controls must have a non-empty accessible name (label, aria-label or aria-labelledby)."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        const CONTROLS: &[&str] = &[
            "textbox",
            "searchbox",
            "combobox",
            "listbox",
            "checkbox",
            "radio",
            "slider",
            "spinbutton",
            "switch",
        ];
        check_named(document, self.id(), CONTROLS, |n| {
            format!(
                "Form control (role: {}) has no accessible name. Add a label, aria-label or \
                 aria-labelledby.",
                n.role
            )
        })
    }
}

/// Buttons need a name.
pub struct EmptyButton;
impl AccessibilityRule for EmptyButton {
    fn id(&self) -> &str {
        "a11y-empty-button"
    }
    fn description(&self) -> &str {
        "Buttons must have a non-empty accessible name."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        check_named(document, self.id(), &["button"], |_| {
            "Button has no accessible name. Add text content, aria-label or aria-labelledby.".into()
        })
    }
}

/// Links need a name.
pub struct EmptyLink;
impl AccessibilityRule for EmptyLink {
    fn id(&self) -> &str {
        "a11y-empty-link"
    }
    fn description(&self) -> &str {
        "Links must have a non-empty accessible name."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        check_named(document, self.id(), &["link"], |_| {
            "Link has no accessible name. Add text content, aria-label or aria-labelledby.".into()
        })
    }
}

/// Headings do not skip levels.
pub struct HeadingHierarchy;
impl AccessibilityRule for HeadingHierarchy {
    fn id(&self) -> &str {
        "a11y-heading-hierarchy"
    }
    fn description(&self) -> &str {
        "Heading levels should not skip (an h1 followed by an h3, with no h2 between)."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        let mut previous: Option<u32> = None;
        let mut found = Vec::new();
        for node in document.nodes().filter(|n| n.role == "heading") {
            let Some(level) = node
                .properties
                .get("level")
                .and_then(|l| l.parse::<u32>().ok())
            else {
                continue;
            };
            if let Some(before) = previous
                && level > before + 1
            {
                found.push(
                    Violation::new(
                        self.id(),
                        Severity::Warning,
                        format!(
                            "Heading level {level} follows level {before}. Do not skip heading \
                             levels."
                        ),
                    )
                    .at_node(node),
                );
            }
            previous = Some(level);
        }
        found
    }
}

/// The page has a `main` landmark.
pub struct MissingLandmark;
impl AccessibilityRule for MissingLandmark {
    fn id(&self) -> &str {
        "a11y-missing-landmark"
    }
    fn description(&self) -> &str {
        "Pages should have a main landmark for screen reader navigation."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        if !document.is_top || document.nodes().any(|n| n.role == "main") {
            return Vec::new();
        }
        vec![Violation::new(
            self.id(),
            Severity::Warning,
            "Page has no main landmark. Add a <main> element or role=\"main\".",
        )]
    }
}

/// The document declares its language.
pub struct MissingLanguage;
impl AccessibilityRule for MissingLanguage {
    fn id(&self) -> &str {
        "a11y-missing-lang"
    }
    fn description(&self) -> &str {
        "The <html> element must have a lang attribute to identify the document's language."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        if !document.is_top
            || document
                .language
                .as_deref()
                .is_some_and(|l| !l.trim().is_empty())
        {
            return Vec::new();
        }
        vec![
            Violation::new(
                self.id(),
                Severity::Error,
                "Document has no lang attribute. Add lang=\"en\" (or its language) to <html>.",
            )
            .at_selector("html"),
        ]
    }
}

/// Each `id` is used once.
pub struct DuplicateId;
impl AccessibilityRule for DuplicateId {
    fn id(&self) -> &str {
        "a11y-duplicate-id"
    }
    fn description(&self) -> &str {
        "Each id attribute value must be unique within the document."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        document
            .duplicate_ids
            .iter()
            .map(|id| {
                Violation::new(
                    self.id(),
                    Severity::Error,
                    format!(
                        "The id \"{id}\" is used more than once. Labels, aria-labelledby and \
                         aria-describedby can only point at one of them."
                    ),
                )
                .at_selector(format!("[id=\"{}\"]", id.replace('"', "\\\"")))
            })
            .collect()
    }
}

/// Text stands out enough from its background.
pub struct ColorContrast;
impl AccessibilityRule for ColorContrast {
    fn id(&self) -> &str {
        "a11y-color-contrast"
    }
    fn description(&self) -> &str {
        "Text must have enough contrast with its background: 4.5:1, or 3:1 for large text."
    }
    fn check(&self, document: &AuditedDocument) -> Vec<Violation> {
        document
            .text
            .iter()
            .filter_map(|sample| {
                // Text on an image or gradient, or in a colour that cannot be read, is left
                // alone rather than judged on a guess.
                if sample.background_image {
                    return None;
                }
                let background = contrast::effective_background(&sample.backgrounds)?;
                let foreground = Rgba::parse(&sample.color)?.over(background);
                let ratio = contrast::ratio(foreground, background);
                let large = contrast::is_large(sample.font_size, sample.font_weight);
                let required = if large {
                    contrast::LARGE_TEXT
                } else {
                    contrast::NORMAL_TEXT
                };
                (ratio < required).then(|| {
                    Violation::new(
                        self.id(),
                        Severity::Error,
                        format!(
                            "Contrast of {ratio:.2}:1 is below {required:.1}:1 for {} text \
                             \"{}\": {} on {}.",
                            if large { "large" } else { "normal" },
                            sample.text,
                            foreground.css(),
                            background.css()
                        ),
                    )
                    .at_selector(&sample.selector)
                })
            })
            .collect()
    }
}

/// The built-in rules, in the order audits run them.
pub fn builtin() -> Vec<std::sync::Arc<dyn AccessibilityRule>> {
    vec![
        std::sync::Arc::new(AltText),
        std::sync::Arc::new(UnlabeledFormControl),
        std::sync::Arc::new(MissingLandmark),
        std::sync::Arc::new(HeadingHierarchy),
        std::sync::Arc::new(EmptyButton),
        std::sync::Arc::new(EmptyLink),
        std::sync::Arc::new(ColorContrast),
        std::sync::Arc::new(DuplicateId),
        std::sync::Arc::new(MissingLanguage),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn n(role: &str, name: &str) -> AxNode {
        AxNode {
            role: role.into(),
            name: name.into(),
            value: None,
            description: String::new(),
            properties: BTreeMap::new(),
            children: Vec::new(),
            backend_node_id: Some(1),
        }
    }

    fn doc(nodes: Vec<AxNode>) -> AuditedDocument {
        let mut root = n("RootWebArea", "");
        root.children = nodes;
        AuditedDocument {
            url: "https://a.test/".into(),
            is_top: true,
            tree: AccessibilitySnapshot {
                roots: vec![root],
                ignored_count: 0,
            },
            language: Some("en".into()),
            ..Default::default()
        }
    }

    fn ids(found: &[Violation]) -> Vec<&str> {
        found.iter().map(|v| v.rule_id.as_str()).collect()
    }

    #[test]
    fn unnamed_images_buttons_links_and_controls_are_found() {
        let mut hidden = n("button", "");
        hidden.properties.insert("hidden".into(), "true".into());
        let d = doc(vec![
            n("image", ""),
            n("img", "Logo"),
            n("button", "  "),
            n("button", "Save"),
            hidden,
            n("link", ""),
            n("textbox", ""),
            n("checkbox", "Agree"),
            n("main", ""),
        ]);
        let found: Vec<Violation> = builtin().iter().flat_map(|r| r.check(&d)).collect();
        assert_eq!(
            ids(&found),
            [
                "a11y-alt-text",
                "a11y-unlabeled-form-control",
                "a11y-empty-button",
                "a11y-empty-link"
            ]
        );
        assert!(found.iter().all(|v| v.severity == Severity::Error));
        assert_eq!(found[1].role.as_deref(), Some("textbox"));
    }

    #[test]
    fn heading_levels_may_go_down_freely_but_not_skip_up() {
        let h = |level: &str| {
            let mut h = n("heading", "x");
            h.properties.insert("level".into(), level.into());
            h
        };
        let d = doc(vec![h("1"), h("2"), h("4"), h("2"), h("3"), h("5")]);
        let found = HeadingHierarchy.check(&d);
        let messages: Vec<&str> = found.iter().map(|v| v.message.as_str()).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(messages[0].starts_with("Heading level 4 follows level 2"));
        assert!(messages[1].starts_with("Heading level 5 follows level 3"));
        assert!(found.iter().all(|v| v.severity == Severity::Warning));
    }

    #[test]
    fn page_wide_rules_apply_to_the_top_document_only() {
        let mut top = doc(vec![n("button", "x")]);
        top.language = None;
        assert_eq!(ids(&MissingLandmark.check(&top)), ["a11y-missing-landmark"]);
        assert_eq!(ids(&MissingLanguage.check(&top)), ["a11y-missing-lang"]);
        let mut frame = top.clone();
        frame.is_top = false;
        assert!(MissingLandmark.check(&frame).is_empty());
        assert!(MissingLanguage.check(&frame).is_empty());
    }

    #[test]
    fn each_duplicate_id_is_its_own_violation() {
        let mut d = doc(vec![]);
        d.duplicate_ids = vec!["email".into(), "a\"b".into()];
        let found = DuplicateId.check(&d);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].selector.as_deref(), Some("[id=\"email\"]"));
        assert_eq!(found[1].selector.as_deref(), Some("[id=\"a\\\"b\"]"));
    }

    fn sample(color: &str, backgrounds: &[&str], size: f64, weight: f64) -> TextSample {
        TextSample {
            selector: "p".into(),
            text: "Hello".into(),
            color: color.into(),
            backgrounds: backgrounds.iter().map(|s| s.to_string()).collect(),
            background_image: false,
            font_size: size,
            font_weight: weight,
        }
    }

    #[test]
    fn contrast_uses_the_background_actually_behind_the_text() {
        let mut d = doc(vec![]);
        d.text = vec![
            // Default black text on transparent layers over the white canvas: fine.
            sample(
                "rgb(0, 0, 0)",
                &["rgba(0, 0, 0, 0)", "rgba(0, 0, 0, 0)"],
                16.0,
                400.0,
            ),
            // Light grey on white: fails.
            sample("rgb(170, 170, 170)", &["rgb(255, 255, 255)"], 16.0, 400.0),
            // The same grey at a large size still fails 3:1 (2.32:1).
            sample("rgb(170, 170, 170)", &["rgb(255, 255, 255)"], 32.0, 400.0),
            // #767676 is fine for normal text.
            sample("#767676", &["#fff"], 16.0, 400.0),
            // White on a dark ancestor's background, through a transparent element: fine.
            sample(
                "rgb(255, 255, 255)",
                &["rgba(0, 0, 0, 0)", "rgb(20, 20, 20)"],
                16.0,
                400.0,
            ),
            // Black text at 30% opacity on white is a light grey, about 2.1:1: fails.
            sample("rgba(0, 0, 0, 0.3)", &["rgb(255, 255, 255)"], 16.0, 400.0),
        ];
        let mut image = sample("rgb(200, 200, 200)", &["rgb(255, 255, 255)"], 16.0, 400.0);
        image.background_image = true;
        d.text.push(image);
        let found = ColorContrast.check(&d);
        let messages: Vec<&str> = found.iter().map(|v| v.message.as_str()).collect();
        assert_eq!(found.len(), 3, "{messages:#?}");
        assert!(messages[0].contains("below 4.5:1 for normal text"));
        assert!(messages[1].contains("below 3.0:1 for large text"));
        assert!(messages[2].contains("rgb(179, 179, 179) on rgb(255, 255, 255)"));
        assert_eq!(found[0].selector.as_deref(), Some("p"));
    }

    #[test]
    fn violations_display_with_their_severity_and_place() {
        let v = Violation::new("a11y-x", Severity::Warning, "Bad.").at_selector("#a");
        assert_eq!(v.to_string(), "[warning] a11y-x: Bad. (#a)");
    }
}

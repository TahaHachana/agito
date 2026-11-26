//! Selector strategies. A selector `prefix=expression` is resolved by the strategy registered
//! under `prefix`; a selector with no prefix is CSS. Built-in strategies are registered by the
//! built-in selectors plugin through the same plugin API third parties use.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use agito_protocol::cdp::{accessibility, dom};
use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::frame::Frame;
use crate::handles::ElementHandle;
use crate::transport::capabilities;

/// Resolves selectors of one kind to elements.
#[async_trait]
pub trait SelectorStrategy: Send + Sync + 'static {
    /// The prefix before `=` that selects this strategy, such as `css` or `xpath`.
    fn name(&self) -> &str;

    /// Higher priorities are tried first when generating selectors for an element.
    fn priority(&self) -> i32 {
        0
    }

    /// Every element in `frame` matching `selector` (the part after `prefix=`),
    /// in document order.
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        pierce_shadow: bool,
    ) -> Result<Vec<ElementHandle>>;

    /// A selector of this kind that identifies `element`, if this strategy can make one.
    async fn generate(&self, _element: &ElementHandle) -> Result<Option<String>> {
        Ok(None)
    }
}

/// The strategies available in a context, by prefix (case-insensitive).
#[derive(Clone, Default)]
pub struct SelectorRegistry {
    strategies: Arc<RwLock<HashMap<String, Arc<dyn SelectorStrategy>>>>,
}

impl SelectorRegistry {
    pub fn register(&self, strategy: Arc<dyn SelectorStrategy>) {
        self.strategies
            .write()
            .unwrap()
            .insert(strategy.name().to_ascii_lowercase(), strategy);
    }

    pub fn get(&self, prefix: &str) -> Option<Arc<dyn SelectorStrategy>> {
        self.strategies
            .read()
            .unwrap()
            .get(&prefix.to_ascii_lowercase())
            .cloned()
    }

    /// All strategies, highest priority first.
    pub fn by_priority(&self) -> Vec<Arc<dyn SelectorStrategy>> {
        let mut all: Vec<_> = self.strategies.read().unwrap().values().cloned().collect();
        all.sort_by_key(|s| std::cmp::Reverse(s.priority()));
        all
    }
}

/// Splits `prefix=expression`. Anything that looks like CSS (a space, bracket, dot, hash,
/// combinator or colon before the `=`) is CSS.
pub fn parse_prefix(selector: &str) -> (&str, &str) {
    let Some(eq) = selector.find('=') else {
        return ("css", selector);
    };
    let candidate = &selector[..eq];
    if candidate.is_empty()
        || candidate.chars().any(|c| {
            matches!(
                c,
                ' ' | '[' | ']' | '.' | '#' | '>' | '~' | '+' | ':' | ',' | '*'
            )
        })
    {
        return ("css", selector);
    }
    (candidate, &selector[eq + 1..])
}

const QUERY_SHADOW: &str = include_str!("../js/query_shadow.js");
const GET_BY: &str = include_str!("../js/get_by.js");

fn js_string(s: &str) -> String {
    Value::from(s).to_string()
}

/// Evaluates an expression returning an array of elements, in `frame`, and splits it into
/// handles.
pub async fn eval_to_handles(frame: &Frame, expression: &str) -> Result<Vec<ElementHandle>> {
    frame.query_all(expression).await
}

/// `css=` and plain selectors.
pub struct CssStrategy;

#[async_trait]
impl SelectorStrategy for CssStrategy {
    fn name(&self) -> &str {
        "css"
    }
    fn priority(&self) -> i32 {
        10
    }
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        pierce: bool,
    ) -> Result<Vec<ElementHandle>> {
        let sel = js_string(selector);
        let js = if pierce {
            format!("(() => {{ {QUERY_SHADOW} return queryShadow(document, {sel}); }})()")
        } else {
            format!("Array.from(document.querySelectorAll({sel}))")
        };
        eval_to_handles(frame, &js).await
    }
    async fn generate(&self, element: &ElementHandle) -> Result<Option<String>> {
        element
            .evaluate(
                r#"function() {
                    var el = this;
                    if (el.id) return '#' + CSS.escape(el.id);
                    var parts = [];
                    while (el && el !== document.body && el !== document.documentElement) {
                        var tag = el.tagName.toLowerCase();
                        if (el.id) { parts.unshift('#' + CSS.escape(el.id)); break; }
                        var classes = Array.from(el.classList).map(c => '.' + CSS.escape(c)).join('');
                        if (classes) { parts.unshift(tag + classes); }
                        else {
                            var parent = el.parentElement;
                            var siblings = parent ? Array.from(parent.children).filter(c => c.tagName === el.tagName) : [];
                            parts.unshift(siblings.length > 1 ? tag + ':nth-of-type(' + (siblings.indexOf(el) + 1) + ')' : tag);
                        }
                        el = el.parentElement;
                    }
                    return 'css=' + parts.join(' > ');
                }"#,
                &[],
            )
            .await
    }
}

/// `xpath=` selectors. XPath cannot see into shadow roots.
pub struct XPathStrategy;

#[async_trait]
impl SelectorStrategy for XPathStrategy {
    fn name(&self) -> &str {
        "xpath"
    }
    fn priority(&self) -> i32 {
        10
    }
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        _pierce: bool,
    ) -> Result<Vec<ElementHandle>> {
        let js = format!(
            "(() => {{ var r = document.evaluate({}, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null); \
             var a = []; for (var i = 0; i < r.snapshotLength; i++) a.push(r.snapshotItem(i)); return a; }})()",
            js_string(selector)
        );
        eval_to_handles(frame, &js).await
    }
}

/// `text=` selectors: elements whose text contains the value, or equals it when quoted.
pub struct TextStrategy;

#[async_trait]
impl SelectorStrategy for TextStrategy {
    fn name(&self) -> &str {
        "text"
    }
    fn priority(&self) -> i32 {
        20
    }
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        pierce: bool,
    ) -> Result<Vec<ElementHandle>> {
        let exact = selector.len() >= 2 && selector.starts_with('"') && selector.ends_with('"');
        let text = if exact {
            &selector[1..selector.len() - 1]
        } else {
            selector
        };
        let t = js_string(text);
        let test = if exact {
            format!("el.textContent && el.textContent.trim() === {t}")
        } else {
            format!("el.textContent && el.textContent.includes({t})")
        };
        // Every element whose text matches contains its matching descendants' text too, so keep
        // only the innermost matches: the elements a reader would point at.
        let innermost = "results.filter(function(el) { return !results.some(function(o) { return o !== el && el.contains(o); }); })";
        let js = if pierce {
            format!(
                "(() => {{ function walk(root) {{ var results = []; var all = root.querySelectorAll('*'); \
                 for (var i = 0; i < all.length; i++) {{ var el = all[i]; \
                 if (el.tagName !== 'SCRIPT' && el.tagName !== 'STYLE' && ({test})) results.push(el); \
                 if (el.shadowRoot) results = results.concat(walk(el.shadowRoot)); }} return results; }} \
                 var results = walk(document); return {innermost}; }})()"
            )
        } else {
            format!(
                "(() => {{ var results = []; var w = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT); \
                 var el; while (el = w.nextNode()) {{ if (el.tagName !== 'SCRIPT' && el.tagName !== 'STYLE' && ({test})) results.push(el); }} \
                 return {innermost}; }})()"
            )
        };
        eval_to_handles(frame, &js).await
    }
    async fn generate(&self, element: &ElementHandle) -> Result<Option<String>> {
        let text = element.text_content().await?.unwrap_or_default();
        let trimmed = text.trim();
        Ok((!trimmed.is_empty() && text.len() <= 100).then(|| format!("text={trimmed}")))
    }
}

/// `data-testid=` selectors (or another attribute name).
pub struct TestIdStrategy {
    attribute: String,
}

impl TestIdStrategy {
    pub fn new(attribute: impl Into<String>) -> Self {
        Self {
            attribute: attribute.into(),
        }
    }
}

#[async_trait]
impl SelectorStrategy for TestIdStrategy {
    fn name(&self) -> &str {
        &self.attribute
    }
    fn priority(&self) -> i32 {
        40
    }
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        pierce: bool,
    ) -> Result<Vec<ElementHandle>> {
        let css = format!("[{}={}]", self.attribute, css_string(selector));
        CssStrategy.resolve(&css, frame, pierce).await
    }
    async fn generate(&self, element: &ElementHandle) -> Result<Option<String>> {
        Ok(element
            .get_attribute(&self.attribute)
            .await?
            .map(|v| format!("{}={v}", self.attribute)))
    }
}

/// `role=button[name="Save"]` selectors, answered from the browser's accessibility tree.
#[derive(Default)]
pub struct RoleStrategy {
    enabled_sessions: Mutex<HashSet<String>>,
}

#[async_trait]
impl SelectorStrategy for RoleStrategy {
    fn name(&self) -> &str {
        "role"
    }
    fn priority(&self) -> i32 {
        30
    }
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        _pierce: bool,
    ) -> Result<Vec<ElementHandle>> {
        let session = &frame.session();
        capabilities::require_accessibility_tree(
            session.capabilities(),
            session.transport_description(),
            "get_by_role and role= selectors",
            Some("Match by label, text, test id or CSS selector instead."),
        )?;
        let (role, name) = parse_role_selector(selector);
        let key = session.session_id().unwrap_or_default().to_owned();
        if !self.enabled_sessions.lock().unwrap().contains(&key) {
            session
                .send(&accessibility::EnableParams::default())
                .await?;
            self.enabled_sessions.lock().unwrap().insert(key);
        }
        let root = frame
            .evaluate_raw("document.documentElement", false)
            .await?;
        let Some(root_id) = root.object_id else {
            return Ok(Vec::new());
        };
        let spellings: &[&str] = if role == "img" || role == "image" {
            &["image", "img"]
        } else {
            &[role.as_str()]
        };
        let mut nodes = Vec::new();
        for spelling in spellings {
            let result = session
                .send(&accessibility::QueryAXTreeParams {
                    object_id: Some(root_id.clone()),
                    accessible_name: name.clone(),
                    role: Some((*spelling).to_owned()),
                    ..Default::default()
                })
                .await?;
            nodes = result.nodes;
            if !nodes.is_empty() {
                break;
            }
        }
        let mut handles = Vec::new();
        for node in nodes {
            let (false, Some(backend)) = (node.ignored, node.backend_dom_node_id) else {
                continue;
            };
            let resolved = session
                .send(&dom::ResolveNodeParams {
                    backend_node_id: Some(backend),
                    ..Default::default()
                })
                .await?;
            if let Some(id) = resolved.object.object_id {
                handles.push(ElementHandle::new(session.clone(), id));
            }
        }
        Ok(handles)
    }
}

/// Splits `button[name="Save"]` into the role and the accessible name.
pub fn parse_role_selector(selector: &str) -> (String, Option<String>) {
    let Some(open) = selector.find('[') else {
        return (selector.to_owned(), None);
    };
    let role = selector[..open].to_owned();
    let Some(rest) = selector[open + 1..].strip_prefix("name=") else {
        return (role, None);
    };
    if rest.len() >= 3 && rest.starts_with('"') && rest.ends_with("\"]") {
        return (role, Some(rest[1..rest.len() - 2].to_owned()));
    }
    let name = rest.split(']').next().unwrap_or(rest);
    (role, Some(name.to_owned()))
}

/// The selector `get_by_role` builds.
pub fn role_selector(role: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("role={role}[name=\"{n}\"]"),
        None => format!("role={role}"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GetByKind {
    Text,
    Label,
    Attribute,
}

/// The `_text`, `_label` and `_attr` strategies behind the `get_by_*` methods.
pub struct GetByStrategy {
    kind: GetByKind,
}

impl GetByStrategy {
    pub fn text() -> Self {
        Self {
            kind: GetByKind::Text,
        }
    }
    pub fn label() -> Self {
        Self {
            kind: GetByKind::Label,
        }
    }
    pub fn attribute() -> Self {
        Self {
            kind: GetByKind::Attribute,
        }
    }
}

#[async_trait]
impl SelectorStrategy for GetByStrategy {
    fn name(&self) -> &str {
        match self.kind {
            GetByKind::Text => "_text",
            GetByKind::Label => "_label",
            GetByKind::Attribute => "_attr",
        }
    }
    async fn resolve(
        &self,
        selector: &str,
        frame: &Frame,
        pierce: bool,
    ) -> Result<Vec<ElementHandle>> {
        let (attribute, text, exact) = parse_get_by(selector, self.kind == GetByKind::Attribute)?;
        let kind = match self.kind {
            GetByKind::Text => "text",
            GetByKind::Label => "label",
            GetByKind::Attribute => "attr",
        };
        let js = format!(
            "{GET_BY}({}, {}, {}, {exact}, {pierce})",
            js_string(kind),
            js_string(attribute.as_deref().unwrap_or("")),
            js_string(&text)
        );
        eval_to_handles(frame, &js).await
    }
}

/// `"text"i` / `"text"s` literals: a JSON string, then `i` (substring, any case) or `s` (exact).
fn format_text(text: &str, exact: bool) -> String {
    format!("{}{}", js_string(text), if exact { "s" } else { "i" })
}

fn parse_get_by(expression: &str, has_attribute: bool) -> Result<(Option<String>, String, bool)> {
    let malformed = || {
        Error::invalid_argument(format!(
            "Malformed locator expression '{expression}': expected a quoted string followed by 'i' or 's'."
        ))
    };
    let (attribute, literal) = if has_attribute {
        let eq = expression
            .find('=')
            .filter(|&i| i > 0)
            .ok_or_else(malformed)?;
        (Some(expression[..eq].to_owned()), &expression[eq + 1..])
    } else {
        (None, expression)
    };
    let exact = match literal.chars().last() {
        Some('s') => true,
        Some('i') => false,
        _ => return Err(malformed()),
    };
    let text: String =
        serde_json::from_str(&literal[..literal.len() - 1]).map_err(|_| malformed())?;
    Ok((attribute, text, exact))
}

pub fn get_by_text(text: &str, exact: bool) -> String {
    format!("_text={}", format_text(text, exact))
}

pub fn get_by_label(text: &str, exact: bool) -> String {
    format!("_label={}", format_text(text, exact))
}

pub fn get_by_attribute(attribute: &str, text: &str, exact: bool) -> String {
    format!("_attr={attribute}={}", format_text(text, exact))
}

pub fn test_id_selector(test_id: &str) -> String {
    format!("[data-testid={}]", css_string(test_id))
}

/// A CSS string literal.
pub fn css_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => out.push_str(&format!("\\{:x} ", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Registers the built-in strategies.
pub(crate) fn register_builtins(registry: &SelectorRegistry) {
    registry.register(Arc::new(CssStrategy));
    registry.register(Arc::new(XPathStrategy));
    registry.register(Arc::new(TextStrategy));
    registry.register(Arc::new(TestIdStrategy::new("data-testid")));
    registry.register(Arc::new(RoleStrategy::default()));
    registry.register(Arc::new(GetByStrategy::text()));
    registry.register(Arc::new(GetByStrategy::label()));
    registry.register(Arc::new(GetByStrategy::attribute()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes() {
        assert_eq!(parse_prefix("#id"), ("css", "#id"));
        assert_eq!(parse_prefix("input[name=q]"), ("css", "input[name=q]"));
        assert_eq!(parse_prefix("a:has-text=x"), ("css", "a:has-text=x"));
        assert_eq!(parse_prefix("text=Hello"), ("text", "Hello"));
        assert_eq!(parse_prefix("xpath=//a"), ("xpath", "//a"));
        assert_eq!(parse_prefix("data-testid=x"), ("data-testid", "x"));
        assert_eq!(
            parse_prefix("_attr=title=\"x\"i"),
            ("_attr", "title=\"x\"i")
        );
    }

    #[test]
    fn roles() {
        assert_eq!(parse_role_selector("button"), ("button".into(), None));
        assert_eq!(
            parse_role_selector("button[name=\"Save it\"]"),
            ("button".into(), Some("Save it".into()))
        );
        assert_eq!(
            parse_role_selector("link[name=Home]"),
            ("link".into(), Some("Home".into()))
        );
        assert_eq!(
            role_selector("button", Some("Go")),
            "role=button[name=\"Go\"]"
        );
    }

    #[test]
    fn get_by_round_trips() {
        let sel = get_by_attribute("placeholder", "Your \"name\"", true);
        let (prefix, expr) = parse_prefix(&sel);
        assert_eq!(prefix, "_attr");
        let (attr, text, exact) = parse_get_by(expr, true).unwrap();
        assert_eq!(attr.as_deref(), Some("placeholder"));
        assert_eq!(text, "Your \"name\"");
        assert!(exact);
        assert!(parse_get_by("\"x\"", false).is_err());
    }

    #[test]
    fn css_strings_escape() {
        assert_eq!(css_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(test_id_selector("x"), "[data-testid=\"x\"]");
    }
}

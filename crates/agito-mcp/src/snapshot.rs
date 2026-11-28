//! The snapshot an agent reads: a compact, indented accessibility tree with a ref on each node
//! worth addressing, and the map from those refs back to elements.
//!
//! The browser's tree says most things three times: a button, the static text inside it, and
//! the inline text box that text is laid out in. The agent needs the button. So layout nodes
//! are never printed, text that only repeats its parent's name is folded away, text that adds
//! something is printed on the parent's line, and an unnamed wrapper with one child steps aside
//! for that child. Refs go only to nodes an agent might act on: interactive roles, named nodes,
//! frames, and anything focusable.

use std::collections::HashMap;

use agito::Frame;
use agito::accessibility::AxNode;

/// Boolean properties printed as `[state]` flags, in this order.
const STATE_PROPERTIES: &[&str] = &[
    "disabled", "readonly", "required", "checked", "selected", "expanded", "pressed",
];

/// Roles an agent acts on. These take a ref even without a name, since an unlabeled control
/// is exactly what an audit points at and a fix needs to reach.
const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "combobox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "tab",
    "option",
    "listbox",
    "menu",
    "tree",
    "treeitem",
    "gridcell",
    "cell",
    "row",
    "iframe",
];

/// Text and label nodes never take a ref: the element they belong to is the thing to address.
const TEXT_ROLES: &[&str] = &["statictext", "inlinetextbox", "listmarker", "labeltext"];

/// Layout detail with nothing to say: the line-broken layout of text, a list item's marker, a
/// line break.
const DROPPED_ROLES: &[&str] = &["inlinetextbox", "listmarker", "linebreak"];

/// The longest inline text reported for a node before it is cut short.
const INLINE_TEXT_LIMIT: usize = 80;

fn is_one_of(role: &str, roles: &[&str]) -> bool {
    roles.iter().any(|r| r.eq_ignore_ascii_case(role))
}

/// Where a ref points: a backend DOM node, and the frame whose document holds it. `None` is
/// the document the snapshot itself covered.
#[derive(Clone)]
pub(crate) struct RefTarget {
    pub backend_node_id: i64,
    pub frame: Option<Frame>,
}

/// One frame's document, printed inside the element that hosts it.
#[derive(Clone)]
pub(crate) struct FrameTree {
    /// The index the frame tools address it by, and the prefix on its refs.
    pub index: usize,
    pub frame: Frame,
    pub roots: Vec<AxNode>,
}

/// The frame documents a page snapshot prints inside itself, found by the element hosting
/// each. A host is named by its document as well as its node, since node ids only mean
/// something within one document; `None` is the document the snapshot covered.
#[derive(Default)]
pub(crate) struct InlineFrames {
    by_host: HashMap<(Option<String>, i64), FrameTree>,
}

impl InlineFrames {
    pub fn add(&mut self, host: Option<&Frame>, host_node: i64, tree: FrameTree) {
        self.by_host
            .insert((host.map(|f| f.id().to_owned()), host_node), tree);
    }

    pub fn len(&self) -> usize {
        self.by_host.len()
    }

    fn find(&self, host: Option<&Frame>, host_node: i64) -> Option<&FrameTree> {
        self.by_host
            .get(&(host.map(|f| f.id().to_owned()), host_node))
    }
}

/// A rendered snapshot: the text, each ref's target, and the frames printed inside it.
pub(crate) struct Serialized {
    pub text: String,
    pub refs: HashMap<String, RefTarget>,
    pub frames: Vec<FrameTree>,
}

fn role_of(node: &AxNode) -> &str {
    if node.role.is_empty() {
        "generic"
    } else {
        &node.role
    }
}

fn is_static_text(node: &AxNode) -> bool {
    node.role.eq_ignore_ascii_case("StaticText")
}

fn is_document(node: &AxNode) -> bool {
    node.role.eq_ignore_ascii_case("RootWebArea")
}

fn is_generic(node: &AxNode) -> bool {
    node.role.is_empty() || is_one_of(&node.role, &["generic", "none", "presentation"])
}

fn is_true(node: &AxNode, property: &str) -> bool {
    node.properties
        .get(property)
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

fn property<'a>(node: &'a AxNode, property: &str) -> Option<&'a str> {
    node.properties
        .get(property)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
}

/// Collapses whitespace runs, newlines included, to one space and trims the ends, so a name
/// always fits on its one line.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escapes a value for printing between double quotes.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Whether a node takes a ref: it maps to a DOM node, is not itself text, and is interactive,
/// named, or focusable.
pub(crate) fn takes_ref(node: &AxNode) -> bool {
    if node.backend_node_id.is_none() || is_one_of(role_of(node), TEXT_ROLES) {
        return false;
    }
    is_one_of(role_of(node), INTERACTIVE_ROLES)
        || !node.name.trim().is_empty()
        || is_true(node, "focusable")
}

/// The first node carrying `backend_node_id`, in document order.
pub(crate) fn find_by_backend_id(roots: &[AxNode], backend_node_id: i64) -> Option<&AxNode> {
    roots
        .iter()
        .flat_map(AxNode::walk)
        .find(|n| n.backend_node_id == Some(backend_node_id))
}

/// The text under a node as the snapshot would print it, cut short past a readable length;
/// `None` when it has none. Describes a node that took no ref.
pub(crate) fn inline_text(node: &AxNode) -> Option<String> {
    fn collect(node: &AxNode, out: &mut String) {
        if is_static_text(node) {
            let text = normalize(&node.name);
            if !text.is_empty() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&text);
            }
            return;
        }
        for child in &node.children {
            if out.len() > INLINE_TEXT_LIMIT {
                return;
            }
            collect(child, out);
        }
    }
    let mut out = String::new();
    collect(node, &mut out);
    if out.is_empty() {
        return None;
    }
    if out.chars().count() <= INLINE_TEXT_LIMIT {
        return Some(out);
    }
    let cut: String = out.chars().take(INLINE_TEXT_LIMIT).collect();
    Some(format!("{}...", cut.trim_end()))
}

/// Renders `roots`. `max_depth` counts printed levels below each root (0 prints only the
/// roots); a wrapper that collapses into its child takes no level. `frames` are printed inside
/// the elements hosting them; `None` prints each frame element as a leaf.
pub(crate) fn serialize(
    roots: &[AxNode],
    max_depth: Option<usize>,
    frames: Option<&InlineFrames>,
) -> Serialized {
    let mut writer = Writer {
        max_depth,
        frames,
        text: String::new(),
        refs: HashMap::new(),
        next_ref: 1,
        frame: None,
        prefix: String::new(),
        printed_frames: Vec::new(),
    };
    for root in roots {
        if let Some(node) = writer.resolve(root) {
            writer.write(node, 0);
        }
    }
    Serialized {
        text: writer.text,
        refs: writer.refs,
        frames: writer.printed_frames,
    }
}

struct Writer<'a> {
    max_depth: Option<usize>,
    frames: Option<&'a InlineFrames>,
    text: String,
    refs: HashMap<String, RefTarget>,
    next_ref: usize,
    /// The document being written, and the prefix its refs carry. The snapshot's own document
    /// is unprefixed; a frame printed inside it numbers its refs from one behind `f<index>`.
    frame: Option<Frame>,
    prefix: String,
    printed_frames: Vec<FrameTree>,
}

impl<'a> Writer<'a> {
    /// The node as it prints: itself, the one child an unnamed wrapper steps aside for, or
    /// nothing at all.
    fn resolve<'n>(&self, node: &'n AxNode) -> Option<&'n AxNode> {
        if is_one_of(role_of(node), DROPPED_ROLES) {
            return None;
        }
        if !is_collapsible(node) {
            return Some(node);
        }
        let children = self.children_of(node);
        match children.len() {
            0 => None,
            1 => Some(children[0]),
            _ => Some(node),
        }
    }

    fn children_of<'n>(&self, node: &'n AxNode) -> Vec<&'n AxNode> {
        node.children
            .iter()
            .filter_map(|child| self.resolve(child))
            .collect()
    }

    fn indent(&mut self, depth: usize) {
        self.text.extend(std::iter::repeat_n(' ', depth * 2));
    }

    fn write(&mut self, node: &AxNode, depth: usize) {
        if self.max_depth.is_some_and(|limit| depth > limit) {
            return;
        }
        if is_static_text(node) {
            // Text reaches here only beside elements, or as a root of its own; a node's only
            // text prints on that node's line instead.
            let text = normalize(&node.name);
            if !text.is_empty() {
                self.indent(depth);
                self.text.push_str("- text: ");
                self.text.push_str(&text);
                self.text.push('\n');
            }
            return;
        }

        self.indent(depth);
        self.text.push_str("- ");
        self.text.push_str(role_of(node));
        let name = normalize(&node.name);
        if !name.is_empty() {
            self.text.push_str(&format!(" \"{}\"", escape(&name)));
        }
        if takes_ref(node) {
            let id = format!("{}e{}", self.prefix, self.next_ref);
            self.next_ref += 1;
            self.refs.insert(
                id.clone(),
                RefTarget {
                    backend_node_id: node.backend_node_id.expect("takes_ref checked it"),
                    frame: self.frame.clone(),
                },
            );
            self.text.push_str(&format!(" [ref={id}]"));
        }
        // The one place the tree crosses into another document: it names the index the frame
        // tools use, which is also the prefix on every ref inside.
        let inner = self.frame_under(node);
        if let Some(inner) = &inner {
            self.text.push_str(&format!(" [frame={}]", inner.index));
        }
        // A list item's level repeats the indentation; a heading's is its outline rank.
        if node.role.eq_ignore_ascii_case("heading")
            && let Some(level) = property(node, "level")
        {
            self.text.push_str(&format!(" [level={level}]"));
        }
        if node.role.eq_ignore_ascii_case("link")
            && let Some(url) = property(node, "url")
        {
            self.text.push_str(&format!(" [url={url}]"));
        }
        let value = normalize(node.value.as_deref().unwrap_or_default());
        if !value.is_empty() {
            self.text
                .push_str(&format!(" [value=\"{}\"]", escape(&value)));
        }
        for state in STATE_PROPERTIES {
            if is_true(node, state) {
                self.text.push_str(&format!(" [{state}]"));
            }
        }

        // Text that only repeats the node's name says where the name came from, and nothing
        // new; text that repeats its value (a field's typed text) is already on the line.
        let remaining: Vec<&AxNode> = self
            .children_of(node)
            .into_iter()
            .filter(|child| {
                if !is_static_text(child) {
                    return true;
                }
                let text = normalize(&child.name);
                !text.is_empty() && text != name && text != value
            })
            .collect();

        if inner.is_none() && !remaining.is_empty() && remaining.iter().all(|c| is_static_text(c)) {
            let joined = remaining
                .iter()
                .map(|c| normalize(&c.name))
                .collect::<Vec<_>>()
                .join(" ");
            if joined != name {
                self.text.push_str(": ");
                self.text.push_str(&joined);
            }
            self.text.push('\n');
            return;
        }

        self.text.push('\n');
        for child in remaining {
            self.write(child, depth + 1);
        }
        if let Some(inner) = inner {
            self.write_frame(inner, depth + 1);
        }
    }

    fn frame_under(&self, node: &AxNode) -> Option<FrameTree> {
        let frames = self.frames?;
        let id = node.backend_node_id?;
        frames.find(self.frame.as_ref(), id).cloned()
    }

    /// Writes a frame's document where its host element stands, numbering its refs from one
    /// again behind the frame's prefix.
    fn write_frame(&mut self, tree: FrameTree, depth: usize) {
        if self.max_depth.is_some_and(|limit| depth > limit) {
            return;
        }
        let outer_frame = self.frame.replace(tree.frame.clone());
        let outer_prefix = std::mem::replace(&mut self.prefix, format!("f{}", tree.index));
        let outer_next = std::mem::replace(&mut self.next_ref, 1);
        self.printed_frames.push(tree.clone());

        // The frame's document node is dropped: its host element has just said a document
        // starts here.
        if tree.roots.len() == 1 && is_document(&tree.roots[0]) {
            for child in self.children_of(&tree.roots[0]) {
                self.write(child, depth);
            }
        } else {
            for root in &tree.roots {
                if let Some(node) = self.resolve(root) {
                    self.write(node, depth);
                }
            }
        }

        self.frame = outer_frame;
        self.prefix = outer_prefix;
        self.next_ref = outer_next;
    }
}

/// An unnamed wrapper with nothing of its own to print: no ref, value or state.
fn is_collapsible(node: &AxNode) -> bool {
    is_generic(node)
        && node.name.trim().is_empty()
        && !takes_ref(node)
        && node.value.as_deref().is_none_or(|v| v.trim().is_empty())
        && !STATE_PROPERTIES.iter().any(|s| is_true(node, s))
}

/// Whether a target is shaped like a ref (`e5`, or `f1e5` inside frame 1) rather than a
/// selector. The shape decides, not the ref map, so a ref the latest snapshot no longer holds
/// is reported stale instead of run as a selector that matches nothing. No CSS selector is a
/// lone letter followed by digits.
pub(crate) fn is_ref_shaped(target: &str) -> bool {
    let rest = match target.strip_prefix('f') {
        Some(after) => {
            let digits = after.chars().take_while(char::is_ascii_digit).count();
            if digits == 0 {
                return false;
            }
            &after[digits..]
        }
        None => target,
    };
    rest.len() >= 2 && rest.starts_with('e') && rest[1..].chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn node(role: &str, name: &str, id: Option<i64>, children: Vec<AxNode>) -> AxNode {
        AxNode {
            role: role.into(),
            name: name.into(),
            value: None,
            description: String::new(),
            properties: BTreeMap::new(),
            children,
            backend_node_id: id,
        }
    }

    fn text(t: &str) -> AxNode {
        node(
            "StaticText",
            t,
            Some(999),
            vec![node("InlineTextBox", t, None, vec![])],
        )
    }

    fn with(mut n: AxNode, key: &str, value: &str) -> AxNode {
        n.properties.insert(key.into(), value.into());
        n
    }

    fn render(roots: &[AxNode]) -> String {
        serialize(roots, None, None).text
    }

    #[test]
    fn text_folds_into_its_element_and_wrappers_step_aside() {
        let tree = node(
            "RootWebArea",
            "Shop",
            Some(1),
            vec![
                node(
                    "generic",
                    "",
                    Some(2),
                    vec![node("button", "Buy", Some(3), vec![text("Buy")])],
                ),
                with(
                    node("heading", "Cart", Some(4), vec![text("Cart")]),
                    "level",
                    "2",
                ),
                node(
                    "list",
                    "",
                    Some(5),
                    vec![
                        node(
                            "listitem",
                            "",
                            Some(6),
                            vec![node("ListMarker", "•", None, vec![]), text("One")],
                        ),
                        node("listitem", "", Some(7), vec![text("Two")]),
                    ],
                ),
                node(
                    "paragraph",
                    "",
                    Some(8),
                    vec![
                        text("Read the"),
                        with(
                            node("link", "terms", Some(9), vec![text("terms")]),
                            "url",
                            "https://shop.test/terms",
                        ),
                        text("first."),
                    ],
                ),
            ],
        );
        assert_eq!(
            render(&[tree]),
            r#"- RootWebArea "Shop" [ref=e1]
  - button "Buy" [ref=e2]
  - heading "Cart" [ref=e3] [level=2]
  - list
    - listitem: One
    - listitem: Two
  - paragraph
    - text: Read the
    - link "terms" [ref=e4] [url=https://shop.test/terms]
    - text: first.
"#
        );
    }

    #[test]
    fn controls_show_values_and_states_and_unnamed_ones_still_take_refs() {
        let mut field = with(
            node("textbox", "Email", Some(2), vec![]),
            "required",
            "true",
        );
        field.value = Some("ada@\nexample.com".into());
        let tree = node(
            "RootWebArea",
            "",
            Some(1),
            vec![
                field,
                with(
                    with(node("checkbox", "", Some(3), vec![]), "checked", "true"),
                    "disabled",
                    "true",
                ),
                with(node("generic", "", Some(4), vec![]), "focusable", "true"),
                node("button", "Say \"hi\"", Some(5), vec![]),
            ],
        );
        assert_eq!(
            render(&[tree]),
            r#"- RootWebArea
  - textbox "Email" [ref=e1] [value="ada@ example.com"] [required]
  - checkbox [ref=e2] [disabled] [checked]
  - generic [ref=e3]
  - button "Say \"hi\"" [ref=e4]
"#
        );
    }

    #[test]
    fn depth_limits_and_frames_print_inside_their_host() {
        let page = node(
            "RootWebArea",
            "",
            Some(1),
            vec![node(
                "main",
                "",
                Some(2),
                vec![node("button", "Deep", Some(3), vec![])],
            )],
        );
        assert_eq!(
            serialize(std::slice::from_ref(&page), Some(1), None).text,
            "- RootWebArea\n  - main\n"
        );

        // A frame host's children come from the frame's own tree, numbered behind f<index>.
        // Frames need a live Frame, so only the leaf form is checked here.
        let host = node("Iframe", "Pay", Some(10), vec![]);
        assert_eq!(render(&[host]), "- Iframe \"Pay\" [ref=e1]\n");
    }

    #[test]
    fn inline_text_is_cut_short() {
        let long = node("paragraph", "", Some(1), vec![text(&"word ".repeat(30))]);
        let got = inline_text(&long).unwrap();
        assert!(got.ends_with("..."), "{got}");
        assert!(got.chars().count() <= INLINE_TEXT_LIMIT + 3);
        assert_eq!(inline_text(&node("img", "", Some(2), vec![])), None);
    }

    #[test]
    fn ref_shapes() {
        for r in ["e1", "e42", "f1e2", "f12e305"] {
            assert!(is_ref_shaped(r), "{r}");
        }
        for s in [
            "e", "f1", "fe2", "#e1", "button", "e1a", "f1x2", "text=e1", "",
        ] {
            assert!(!is_ref_shaped(s), "{s}");
        }
    }
}

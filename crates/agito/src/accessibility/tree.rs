//! Building a tree from the browser's flat list of accessibility nodes.

use std::collections::{BTreeMap, HashMap, HashSet};

use agito_protocol::cdp::accessibility::{AXNode as RawNode, AXValue};
use serde::Serialize;

/// One node of the accessibility tree: what assistive technology knows about an element, or a
/// piece of text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AxNode {
    /// The computed role, such as `button` or `heading`; `StaticText` for a run of text.
    pub role: String,
    /// The accessible name; empty when there is none.
    pub name: String,
    /// The current value of a form control, if it has one.
    pub value: Option<String>,
    /// The accessible description; empty when there is none.
    pub description: String,
    /// Further properties by name, such as `level` for a heading or `checked` for a checkbox.
    pub properties: BTreeMap<String, String>,
    /// Child nodes, in order.
    pub children: Vec<AxNode>,
    /// The DOM node behind it, for finding the element; `None` for nodes with no element.
    pub backend_node_id: Option<i64>,
}

impl AxNode {
    /// This node and all below it, depth first.
    pub fn walk(&self) -> impl Iterator<Item = &AxNode> {
        let mut stack = vec![self];
        std::iter::from_fn(move || {
            let node = stack.pop()?;
            stack.extend(node.children.iter().rev());
            Some(node)
        })
    }
}

/// A document's accessibility tree, without the nodes the browser ignores.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AccessibilitySnapshot {
    /// The top-level nodes; usually one, the document.
    pub roots: Vec<AxNode>,
    /// How many nodes the browser left out as meaningless to assistive technology, such as
    /// layout wrappers.
    pub ignored_count: usize,
}

impl AccessibilitySnapshot {
    /// Every node, depth first.
    pub fn nodes(&self) -> impl Iterator<Item = &AxNode> {
        self.roots.iter().flat_map(AxNode::walk)
    }
}

fn text(value: Option<&AXValue>) -> Option<String> {
    let value = value?.value.as_ref()?;
    Some(match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Builds the tree from the browser's flat list. Ignored nodes are dropped, and a node the
/// browser ignored stands in for its own non-ignored descendants, which take its place in its
/// parent's children: browsers ignore many layout wrappers, and skipping one outright would cut
/// everything under it off the tree.
pub(crate) fn build(raw: Vec<RawNode>) -> AccessibilitySnapshot {
    let ignored_count = raw.iter().filter(|n| n.ignored).count();
    let by_id: HashMap<&str, &RawNode> = raw.iter().map(|n| (n.node_id.as_str(), n)).collect();

    fn convert(
        id: &str,
        by_id: &HashMap<&str, &RawNode>,
        visited: &mut HashSet<String>,
    ) -> Vec<AxNode> {
        // Trees from the browser are acyclic; the guard turns a malformed one into a short
        // tree, not an endless walk.
        if !visited.insert(id.to_owned()) {
            return Vec::new();
        }
        let Some(raw) = by_id.get(id) else {
            return Vec::new();
        };
        let children: Vec<AxNode> = raw
            .child_ids
            .iter()
            .flatten()
            .flat_map(|child| convert(child, by_id, visited))
            .collect();
        if raw.ignored {
            return children;
        }
        let properties = raw
            .properties
            .iter()
            .flatten()
            .filter_map(|p| {
                let name = serde_json::to_value(p.name).ok()?.as_str()?.to_owned();
                Some((name, text(Some(&p.value)).unwrap_or_default()))
            })
            .collect();
        vec![AxNode {
            role: text(raw.role.as_ref()).unwrap_or_default(),
            name: text(raw.name.as_ref()).unwrap_or_default(),
            value: text(raw.value.as_ref()),
            description: text(raw.description.as_ref()).unwrap_or_default(),
            properties,
            children,
            backend_node_id: raw.backend_dom_node_id,
        }]
    }

    let mut visited = HashSet::new();
    let mut roots = Vec::new();
    // Roots are nodes with no parent in the list; anything a walk from them does not reach
    // becomes a root of its own rather than disappearing.
    for node in raw.iter().filter(|n| {
        n.parent_id
            .as_deref()
            .is_none_or(|p| !by_id.contains_key(p))
    }) {
        roots.extend(convert(&node.node_id, &by_id, &mut visited));
    }
    for node in &raw {
        if !visited.contains(&node.node_id) {
            roots.extend(convert(&node.node_id, &by_id, &mut visited));
        }
    }
    AccessibilitySnapshot {
        roots,
        ignored_count,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn raw(nodes: serde_json::Value) -> Vec<RawNode> {
        serde_json::from_value(nodes).unwrap()
    }

    fn node(
        id: &str,
        role: &str,
        name: &str,
        parent: Option<&str>,
        children: &[&str],
        ignored: bool,
    ) -> serde_json::Value {
        json!({
            "nodeId": id, "ignored": ignored,
            "role": { "type": "role", "value": role },
            "name": { "type": "computedString", "value": name },
            "parentId": parent, "childIds": children, "backendDOMNodeId": 1,
        })
    }

    #[test]
    fn ignored_wrappers_are_replaced_by_their_children() {
        let snapshot = build(raw(json!([
            node("1", "RootWebArea", "Shop", None, &["2"], false),
            node("2", "generic", "", Some("1"), &["3", "4"], true),
            node("3", "button", "Buy", Some("2"), &[], false),
            node("4", "generic", "", Some("2"), &["5"], true),
            node("5", "link", "Home", Some("4"), &[], false),
        ])));
        assert_eq!(snapshot.ignored_count, 2);
        assert_eq!(snapshot.roots.len(), 1);
        let roles: Vec<&str> = snapshot.nodes().map(|n| n.role.as_str()).collect();
        assert_eq!(roles, ["RootWebArea", "button", "link"]);
        assert_eq!(
            snapshot.roots[0].children.len(),
            2,
            "both lifted to the root"
        );
    }

    #[test]
    fn properties_and_values_are_kept() {
        let mut heading = node("2", "heading", "Orders", Some("1"), &[], false);
        heading["properties"] =
            json!([{ "name": "level", "value": { "type": "integer", "value": 2 } }]);
        let snapshot = build(raw(json!([
            node("1", "RootWebArea", "", None, &["2"], false),
            heading
        ])));
        let h = snapshot.nodes().nth(1).unwrap();
        assert_eq!(h.properties.get("level").map(String::as_str), Some("2"));
    }

    #[test]
    fn unreachable_nodes_become_roots() {
        let snapshot = build(raw(json!([
            node("1", "RootWebArea", "", None, &[], false),
            node("9", "button", "Stray", Some("1"), &[], false),
        ])));
        let names: Vec<&str> = snapshot.nodes().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["", "Stray"]);
    }
}

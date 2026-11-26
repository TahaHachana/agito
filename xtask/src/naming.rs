//! Conversions from CDP names to Rust identifiers.

/// `backendDOMNodeId` -> `backend_dom_node_id`, `DOMDebugger` -> `dom_debugger`.
pub fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' || c == '.' || c == ' ' {
            if !out.ends_with('_') {
                out.push('_');
            }
            continue;
        }
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let boundary = prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase() && next_lower);
            if boundary && !out.ends_with('_') {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// `parser-blocking` -> `ParserBlocking`, `navigate` -> `Navigate`.
pub fn pascal_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = true;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            upper_next = true;
            continue;
        }
        if upper_next {
            out.push(c.to_ascii_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

const KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield",
];

/// A field or module identifier that is safe to use in Rust source.
pub fn field_ident(name: &str) -> String {
    let snake = snake_case(name);
    match snake.as_str() {
        "self" | "super" | "crate" | "Self" => format!("{snake}_"),
        s if KEYWORDS.contains(&s) => format!("r#{snake}"),
        _ => snake,
    }
}

/// A variant identifier for an enum string value.
pub fn variant_ident(value: &str) -> String {
    let mut name = pascal_case(value);
    if name.is_empty() {
        name = "Empty".to_owned();
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, 'V');
    }
    if name == "Self" {
        name.push('_');
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_handles_acronyms() {
        assert_eq!(snake_case("backendDOMNodeId"), "backend_dom_node_id");
        assert_eq!(snake_case("DOMDebugger"), "dom_debugger");
        assert_eq!(snake_case("IndexedDB"), "indexed_db");
        assert_eq!(snake_case("CSS"), "css");
        assert_eq!(snake_case("frameId"), "frame_id");
        assert_eq!(snake_case("x"), "x");
        assert_eq!(snake_case("WebAuthn"), "web_authn");
    }

    #[test]
    fn identifiers_avoid_keywords() {
        assert_eq!(field_ident("type"), "r#type");
        assert_eq!(field_ident("self"), "self_");
        assert_eq!(field_ident("override"), "r#override");
    }

    #[test]
    fn variants() {
        assert_eq!(variant_ident("parser-blocking"), "ParserBlocking");
        assert_eq!(variant_ident("DOMContentLoaded"), "DOMContentLoaded");
        assert_eq!(variant_ident("0"), "V0");
        assert_eq!(variant_ident(""), "Empty");
        assert_eq!(variant_ident("self"), "Self_");
    }
}

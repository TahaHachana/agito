//! Emits Rust bindings for the Chrome DevTools Protocol.
//!
//! Layout: one module per domain (`cdp/page.rs`, ...). Every CDP type becomes a struct,
//! enum or type alias; every command becomes a `*Params` struct implementing
//! [`Command`](agito_protocol::Command) plus a `*Returns` struct; every event becomes an
//! `*Event` struct implementing [`Event`](agito_protocol::Event).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;

use crate::naming::{field_ident, pascal_case, snake_case, variant_ident};
use crate::schema::{Domain, Prop, TypeDef};

/// What a named CDP type turns into.
#[derive(Debug, Clone)]
enum Kind {
    Struct,
    Enum,
    /// A type alias to the given Rust type, written relative to the domain module.
    Alias,
}

struct TypeInfo<'a> {
    kind: Kind,
    def: &'a TypeDef,
}

/// Fully qualified CDP type name, `Domain.Id`.
type Qualified = String;

struct Ctx<'a> {
    types: HashMap<Qualified, TypeInfo<'a>>,
    /// Struct-to-struct edges that would need a `Box` because they close a cycle.
    boxed_edges: HashSet<(Qualified, Qualified)>,
    defaultable: HashMap<Qualified, bool>,
}

pub fn generate(domains: &[Domain]) -> BTreeMap<String, String> {
    let mut types = HashMap::new();
    for domain in domains {
        for def in &domain.types {
            let kind = if def.kind == "object" && def.properties.is_some() {
                Kind::Struct
            } else if def.kind == "string" && def.enum_values.is_some() {
                Kind::Enum
            } else {
                Kind::Alias
            };
            types.insert(
                format!("{}.{}", domain.domain, def.id),
                TypeInfo { kind, def },
            );
        }
    }
    let mut ctx = Ctx {
        types,
        boxed_edges: HashSet::new(),
        defaultable: HashMap::new(),
    };
    ctx.compute_boxed_edges();
    let names: Vec<Qualified> = ctx.types.keys().cloned().collect();
    for name in names {
        ctx.is_defaultable(&name, &mut HashSet::new());
    }

    let mut files = BTreeMap::new();
    let mut mod_rs = String::new();
    writeln!(
        mod_rs,
        "//! Generated Chrome DevTools Protocol bindings. Do not edit by hand;"
    )
    .unwrap();
    writeln!(mod_rs, "//! run `cargo xtask codegen` instead.").unwrap();
    writeln!(mod_rs).unwrap();
    let mut sorted: Vec<&Domain> = domains.iter().collect();
    sorted.sort_by(|a, b| a.domain.cmp(&b.domain));
    for domain in sorted {
        let module = snake_case(&domain.domain);
        write_docs(
            &mut mod_rs,
            domain.description.as_deref(),
            domain.experimental,
            domain.deprecated,
        );
        writeln!(mod_rs, "pub mod {module};").unwrap();
        files.insert(format!("{module}.rs"), emit_domain(&ctx, domain));
    }
    files.insert("mod.rs".to_owned(), mod_rs);
    files
}

impl Ctx<'_> {
    fn resolve(&self, domain: &str, reference: &str) -> Qualified {
        if reference.contains('.') {
            reference.to_owned()
        } else {
            format!("{domain}.{reference}")
        }
    }

    /// Direct (non-`Vec`) struct-to-struct references, per struct.
    fn direct_edges(&self) -> HashMap<Qualified, Vec<Qualified>> {
        let mut edges: HashMap<Qualified, Vec<Qualified>> = HashMap::new();
        for (name, info) in &self.types {
            if !matches!(info.kind, Kind::Struct) {
                continue;
            }
            let domain = name.split('.').next().unwrap();
            for prop in info.def.properties.as_deref().unwrap_or_default() {
                if let Some(r) = &prop.reference {
                    let target = self.resolve(domain, r);
                    if matches!(self.types.get(&target).map(|t| &t.kind), Some(Kind::Struct)) {
                        edges.entry(name.clone()).or_default().push(target);
                    }
                }
            }
        }
        edges
    }

    fn compute_boxed_edges(&mut self) {
        let edges = self.direct_edges();
        for (from, targets) in &edges {
            for to in targets {
                if reaches(&edges, to, from) {
                    self.boxed_edges.insert((from.clone(), to.clone()));
                }
            }
        }
    }

    fn is_defaultable(&mut self, name: &str, visiting: &mut HashSet<String>) -> bool {
        if let Some(&known) = self.defaultable.get(name) {
            return known;
        }
        let Some(info) = self.types.get(name) else {
            return false;
        };
        if !visiting.insert(name.to_owned()) {
            return false;
        }
        let def = info.def;
        let result = match info.kind {
            Kind::Enum => false,
            Kind::Alias => true,
            Kind::Struct => {
                let domain = name.split('.').next().unwrap().to_owned();
                def.properties
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .all(|p| p.optional || self.prop_defaultable(&domain, p, visiting))
            }
        };
        visiting.remove(name);
        self.defaultable.insert(name.to_owned(), result);
        result
    }

    fn prop_defaultable(
        &mut self,
        domain: &str,
        prop: &Prop,
        visiting: &mut HashSet<String>,
    ) -> bool {
        if let Some(r) = &prop.reference {
            let target = self.resolve(domain, r);
            return self.is_defaultable(&target, visiting);
        }
        prop.enum_values.is_none()
    }

    fn params_defaultable(&self, domain: &str, props: &[Prop]) -> bool {
        props.iter().all(|p| {
            p.optional
                || match &p.reference {
                    Some(r) => *self
                        .defaultable
                        .get(&self.resolve(domain, r))
                        .unwrap_or(&false),
                    None => p.enum_values.is_none(),
                }
        })
    }
}

fn reaches(edges: &HashMap<Qualified, Vec<Qualified>>, start: &str, goal: &str) -> bool {
    let mut stack = vec![start.to_owned()];
    let mut seen = HashSet::new();
    while let Some(node) = stack.pop() {
        if node == goal {
            return true;
        }
        if !seen.insert(node.clone()) {
            continue;
        }
        if let Some(next) = edges.get(&node) {
            stack.extend(next.iter().cloned());
        }
    }
    false
}

/// Emission state for one domain module.
struct DomainEmitter<'c, 'a> {
    ctx: &'c Ctx<'a>,
    domain: &'a str,
    out: String,
    /// Inline enums discovered while emitting fields, written at the end of the module.
    inline_enums: Vec<(String, Vec<String>)>,
    used_names: HashSet<String>,
}

fn emit_domain(ctx: &Ctx<'_>, domain: &Domain) -> String {
    let mut e = DomainEmitter {
        ctx,
        domain: &domain.domain,
        out: String::new(),
        inline_enums: Vec::new(),
        used_names: domain.types.iter().map(|t| t.id.clone()).collect(),
    };
    writeln!(
        e.out,
        "//! `{}` domain. Generated by `cargo xtask codegen`; do not edit.",
        domain.domain
    )
    .unwrap();
    if let Some(desc) = &domain.description {
        writeln!(e.out, "//!").unwrap();
        for line in desc.lines() {
            writeln!(e.out, "//! {}", line.trim()).unwrap();
        }
    }
    writeln!(e.out).unwrap();
    writeln!(
        e.out,
        "#![allow(non_camel_case_types, clippy::all, rustdoc::all)]"
    )
    .unwrap();
    writeln!(e.out).unwrap();
    writeln!(e.out, "use serde::{{Deserialize, Serialize}};").unwrap();
    writeln!(e.out).unwrap();

    for def in &domain.types {
        e.emit_type(def);
    }
    for cmd in &domain.commands {
        let base = pascal_case(&cmd.name);
        let method = format!("{}.{}", domain.domain, cmd.name);
        let params = format!("{base}Params");
        let returns = format!("{base}Returns");
        let mut doc = format!("Parameters of `{method}`.");
        if let Some(d) = &cmd.description {
            doc.push_str("\n\n");
            doc.push_str(d);
        }
        let default = ctx.params_defaultable(&domain.domain, &cmd.parameters);
        e.emit_struct(
            &params,
            Some(&doc),
            cmd.experimental,
            cmd.deprecated,
            &cmd.parameters,
            default,
        );
        let returns_default = ctx.params_defaultable(&domain.domain, &cmd.returns);
        e.emit_struct(
            &returns,
            Some(&format!("Return value of `{method}`.")),
            false,
            false,
            &cmd.returns,
            returns_default,
        );
        writeln!(
            e.out,
            "impl crate::Command for {params} {{\n    const METHOD: &'static str = \"{method}\";\n    type Response = {returns};\n}}\n"
        )
        .unwrap();
    }
    for evt in &domain.events {
        let name = format!("{}Event", pascal_case(&evt.name));
        let method = format!("{}.{}", domain.domain, evt.name);
        let mut doc = format!("Payload of the `{method}` event.");
        if let Some(d) = &evt.description {
            doc.push_str("\n\n");
            doc.push_str(d);
        }
        let default = ctx.params_defaultable(&domain.domain, &evt.parameters);
        e.emit_struct(
            &name,
            Some(&doc),
            evt.experimental,
            evt.deprecated,
            &evt.parameters,
            default,
        );
        writeln!(
            e.out,
            "impl crate::Event for {name} {{\n    const NAME: &'static str = \"{method}\";\n}}\n"
        )
        .unwrap();
    }

    let enums = std::mem::take(&mut e.inline_enums);
    for (name, values) in enums {
        e.emit_enum(&name, None, false, false, &values);
    }
    e.out
}

impl DomainEmitter<'_, '_> {
    fn emit_type(&mut self, def: &TypeDef) {
        let qualified = format!("{}.{}", self.domain, def.id);
        let info = &self.ctx.types[&qualified];
        match info.kind {
            Kind::Enum => {
                self.emit_enum(
                    &def.id,
                    def.description.as_deref(),
                    def.experimental,
                    def.deprecated,
                    def.enum_values.as_ref().unwrap(),
                );
            }
            Kind::Struct => {
                let default = self.ctx.defaultable[&qualified];
                self.emit_struct(
                    &def.id,
                    def.description.as_deref(),
                    def.experimental,
                    def.deprecated,
                    def.properties.as_ref().unwrap(),
                    default,
                );
            }
            Kind::Alias => {
                let target = match def.kind.as_str() {
                    "array" => {
                        let items = def.items.as_ref().expect("array type has items");
                        let item = self.rust_type(&def.id, "Item", items, None);
                        format!("Vec<{item}>")
                    }
                    other => primitive(other).to_owned(),
                };
                write_docs(
                    &mut self.out,
                    def.description.as_deref(),
                    def.experimental,
                    def.deprecated,
                );
                writeln!(self.out, "pub type {} = {};\n", def.id, target).unwrap();
            }
        }
    }

    fn emit_enum(
        &mut self,
        name: &str,
        doc: Option<&str>,
        experimental: bool,
        deprecated: bool,
        values: &[String],
    ) {
        write_docs(&mut self.out, doc, experimental, deprecated);
        writeln!(
            self.out,
            "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]"
        )
        .unwrap();
        writeln!(self.out, "pub enum {name} {{").unwrap();
        let mut seen = HashSet::new();
        for value in values {
            let mut variant = variant_ident(value);
            let mut n = 2;
            while !seen.insert(variant.clone()) {
                variant = format!("{}{n}", variant_ident(value));
                n += 1;
            }
            writeln!(self.out, "    #[serde(rename = {value:?})]\n    {variant},").unwrap();
        }
        writeln!(
            self.out,
            "    /// A value this version of the bindings does not know about.\n    #[serde(other)]\n    UnrecognizedValue,"
        )
        .unwrap();
        writeln!(self.out, "}}\n").unwrap();
    }

    fn emit_struct(
        &mut self,
        name: &str,
        doc: Option<&str>,
        experimental: bool,
        deprecated: bool,
        props: &[Prop],
        default: bool,
    ) {
        let mut fields = String::new();
        let mut seen = HashSet::new();
        for prop in props {
            let mut ident = field_ident(&prop.name);
            if !seen.insert(ident.clone()) {
                ident.push('_');
                seen.insert(ident.clone());
            }
            let mut ty = self.rust_type(name, &prop.name, prop, Some(name));
            write_docs_indented(
                &mut fields,
                prop.description.as_deref(),
                prop.experimental,
                prop.deprecated,
            );
            if prop.optional {
                ty = format!("Option<{ty}>");
                writeln!(
                    fields,
                    "    #[serde(rename = {:?}, default, skip_serializing_if = \"Option::is_none\")]",
                    prop.name
                )
                .unwrap();
            } else {
                writeln!(fields, "    #[serde(rename = {:?})]", prop.name).unwrap();
            }
            writeln!(fields, "    pub {ident}: {ty},").unwrap();
        }
        write_docs(&mut self.out, doc, experimental, deprecated);
        let derives = if default {
            "Debug, Clone, PartialEq, Default, Serialize, Deserialize"
        } else {
            "Debug, Clone, PartialEq, Serialize, Deserialize"
        };
        writeln!(self.out, "#[derive({derives})]").unwrap();
        writeln!(self.out, "pub struct {name} {{\n{fields}}}\n").unwrap();
    }

    /// The Rust type of a property. `owner` names the struct holding it, used for inline
    /// enum names and for deciding whether a reference must be boxed.
    fn rust_type(
        &mut self,
        owner: &str,
        prop_name: &str,
        prop: &Prop,
        box_owner: Option<&str>,
    ) -> String {
        if let Some(r) = &prop.reference {
            let target = self.ctx.resolve(self.domain, r);
            let path = self.type_path(&target);
            if let Some(owner) = box_owner {
                let from = format!("{}.{}", self.domain, owner);
                if self.ctx.boxed_edges.contains(&(from, target)) {
                    return format!("Box<{path}>");
                }
            }
            return path;
        }
        let kind = prop.kind.as_deref().unwrap_or("any");
        if let ("string", Some(values)) = (kind, &prop.enum_values) {
            let mut name = format!("{owner}{}", pascal_case(prop_name));
            while self.used_names.contains(&name) {
                name.push_str("Enum");
            }
            self.used_names.insert(name.clone());
            self.inline_enums.push((name.clone(), values.clone()));
            return name;
        }
        if kind == "array" {
            let items = prop.items.as_ref().expect("array property has items");
            let item_name = format!("{}Item", pascal_case(prop_name));
            let item = self.rust_type(owner, &item_name, items, None);
            return format!("Vec<{item}>");
        }
        primitive(kind).to_owned()
    }

    fn type_path(&self, qualified: &str) -> String {
        let (domain, id) = qualified.split_once('.').expect("qualified name");
        if domain == self.domain {
            id.to_owned()
        } else {
            format!("super::{}::{}", snake_case(domain), id)
        }
    }
}

fn primitive(kind: &str) -> &'static str {
    match kind {
        "string" | "binary" => "String",
        "integer" => "i64",
        "number" => "f64",
        "boolean" => "bool",
        _ => "serde_json::Value",
    }
}

fn write_docs(out: &mut String, doc: Option<&str>, experimental: bool, deprecated: bool) {
    write_docs_prefixed(out, "", doc, experimental, deprecated);
}

fn write_docs_indented(out: &mut String, doc: Option<&str>, experimental: bool, deprecated: bool) {
    write_docs_prefixed(out, "    ", doc, experimental, deprecated);
}

fn write_docs_prefixed(
    out: &mut String,
    indent: &str,
    doc: Option<&str>,
    experimental: bool,
    deprecated: bool,
) {
    let mut wrote = false;
    if let Some(doc) = doc {
        for line in doc.lines() {
            // Trimmed so indented description lines are not read as doctest code blocks.
            let line = line.trim();
            if line.is_empty() {
                writeln!(out, "{indent}///").unwrap();
            } else {
                writeln!(out, "{indent}/// {line}").unwrap();
            }
            wrote = true;
        }
    }
    let mut tags = Vec::new();
    if experimental {
        tags.push("**Experimental.**");
    }
    if deprecated {
        tags.push("**Deprecated.**");
    }
    if !tags.is_empty() {
        if wrote {
            writeln!(out, "{indent}///").unwrap();
        }
        writeln!(out, "{indent}/// {}", tags.join(" ")).unwrap();
    }
}

//! Serde model of the devtools-protocol JSON files.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Protocol {
    pub domains: Vec<Domain>,
}

#[derive(Debug, Deserialize)]
pub struct Domain {
    pub domain: String,
    pub description: Option<String>,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub types: Vec<TypeDef>,
    #[serde(default)]
    pub commands: Vec<CommandDef>,
    #[serde(default)]
    pub events: Vec<EventDef>,
}

#[derive(Debug, Deserialize)]
pub struct TypeDef {
    pub id: String,
    pub description: Option<String>,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<String>>,
    pub properties: Option<Vec<Prop>>,
    pub items: Option<Box<Prop>>,
}

/// A property, parameter, return value or array item.
#[derive(Debug, Deserialize)]
pub struct Prop {
    #[serde(default)]
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<String>>,
    pub items: Option<Box<Prop>>,
}

#[derive(Debug, Deserialize)]
pub struct CommandDef {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub parameters: Vec<Prop>,
    #[serde(default)]
    pub returns: Vec<Prop>,
}

#[derive(Debug, Deserialize)]
pub struct EventDef {
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default)]
    pub parameters: Vec<Prop>,
}

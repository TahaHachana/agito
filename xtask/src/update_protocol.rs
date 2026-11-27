//! `cargo xtask update-protocol`: fetches a `devtools-protocol` release from npm, reports how it
//! differs from the vendored definitions, then vendors it and regenerates the bindings.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha512};

const FILES: [&str; 2] = ["browser_protocol.json", "js_protocol.json"];

pub struct Options {
    /// An npm version or dist-tag; `latest` by default.
    pub version: String,
    /// Report the differences without writing anything.
    pub dry_run: bool,
}

pub fn run(options: &Options, protocol_dir: &Path) -> Result<Outcome, String> {
    let current = std::fs::read_to_string(protocol_dir.join("VERSION"))
        .map(|v| v.trim().to_string())
        .ok();

    let meta_url = format!(
        "https://registry.npmjs.org/devtools-protocol/{}",
        options.version
    );
    let meta: Value =
        serde_json::from_str(&get_text(&meta_url)?).map_err(|e| format!("{meta_url}: {e}"))?;
    let field = |path: &str| {
        meta.pointer(path)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("{meta_url} has no {path}"))
    };
    let (version, tarball, integrity) = (
        field("/version")?,
        field("/dist/tarball")?,
        field("/dist/integrity")?,
    );

    if current.as_deref() == Some(version.as_str()) {
        println!("The vendored protocol is already devtools-protocol {version}.");
        return Ok(Outcome::Unchanged);
    }
    println!(
        "devtools-protocol {} -> {version}",
        current.as_deref().unwrap_or("(unknown version)")
    );

    let archive = get_bytes(&tarball)?;
    verify_integrity(&archive, &integrity).map_err(|e| format!("{tarball}: {e}"))?;
    let fetched = extract(&archive).map_err(|e| format!("{tarball}: {e}"))?;

    let mut changed = false;
    for (name, text) in FILES.iter().zip(&fetched) {
        let path = protocol_dir.join(name);
        let old = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let diff = diff(&parse(&old, name)?, &parse(text, name)?);
        changed |= !diff.is_empty();
        print!("{}", diff.render(name));
    }

    if options.dry_run {
        println!("Dry run: nothing written.");
        return Ok(Outcome::DryRun);
    }
    for (name, text) in FILES.iter().zip(&fetched) {
        write(&protocol_dir.join(name), text)?;
    }
    write(&protocol_dir.join("VERSION"), &format!("{version}\n"))?;
    Ok(if changed {
        Outcome::Updated
    } else {
        Outcome::UpdatedWithoutChanges
    })
}

/// What `run` did, so the caller knows whether to regenerate.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The vendored version is the one asked for.
    Unchanged,
    DryRun,
    /// New definitions were written.
    Updated,
    /// A new version was written whose definitions are the same.
    UpdatedWithoutChanges,
}

fn get_text(url: &str) -> Result<String, String> {
    ureq::get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))
}

fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    ureq::get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Checks `bytes` against an npm `dist.integrity` value, such as `sha512-<base64>`.
fn verify_integrity(bytes: &[u8], integrity: &str) -> Result<(), String> {
    // A value can list several hashes; npm's lists one, and sha512 is the one checked here.
    let expected = integrity
        .split_whitespace()
        .find_map(|h| h.strip_prefix("sha512-"))
        .ok_or_else(|| format!("no sha512 hash in the integrity value '{integrity}'"))?;
    let actual = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err("the download does not match the registry's sha512 hash".into())
    }
}

/// The two protocol files from an npm tarball, in the order of [`FILES`].
fn extract(archive: &[u8]) -> Result<[String; 2], String> {
    let mut found: [Option<String>; 2] = [None, None];
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if let Some(slot) = FILES
            .iter()
            .position(|f| path == Path::new("package/json").join(f))
        {
            let mut text = String::new();
            entry.read_to_string(&mut text).map_err(|e| e.to_string())?;
            found[slot] = Some(text);
        }
    }
    let [Some(browser), Some(js)] = found else {
        return Err(format!(
            "the package lacks json/{}",
            FILES.join(" or json/")
        ));
    };
    Ok([browser, js])
}

fn parse(text: &str, name: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{name}: {e}"))
}

/// What changed in one kind of item (commands, events or types) of one domain.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ItemDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Present in both, with a different definition: parameters, fields, flags, descriptions.
    pub changed: Vec<String>,
}

impl ItemDiff {
    fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub added_domains: Vec<String>,
    pub removed_domains: Vec<String>,
    /// Per domain present in both: commands, events and types, in that order.
    pub domains: BTreeMap<String, [ItemDiff; 3]>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added_domains.is_empty() && self.removed_domains.is_empty() && self.domains.is_empty()
    }

    fn render(&self, file: &str) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        if self.is_empty() {
            let _ = writeln!(out, "  {file}: no changes");
            return out;
        }
        let _ = writeln!(out, "  {file}:");
        for d in &self.added_domains {
            let _ = writeln!(out, "    + domain {d}");
        }
        for d in &self.removed_domains {
            let _ = writeln!(out, "    - domain {d}");
        }
        for (domain, kinds) in &self.domains {
            let _ = writeln!(out, "    ~ {domain}");
            for (kind, diff) in ["command", "event", "type"].iter().zip(kinds) {
                for (mark, names) in [
                    ("+", &diff.added),
                    ("-", &diff.removed),
                    ("~", &diff.changed),
                ] {
                    for name in names {
                        let _ = writeln!(out, "      {mark} {kind} {name}");
                    }
                }
            }
        }
        out
    }
}

/// Compares two protocol documents domain by domain. Unlike Motus, which listed only added and
/// removed commands and events, this also reports types, and items whose definition changed,
/// since those are what change the generated code.
pub fn diff(old: &Value, new: &Value) -> Diff {
    let (old, new) = (domains(old), domains(new));
    let mut diff = Diff {
        added_domains: new
            .keys()
            .filter(|d| !old.contains_key(*d))
            .cloned()
            .collect(),
        removed_domains: old
            .keys()
            .filter(|d| !new.contains_key(*d))
            .cloned()
            .collect(),
        domains: BTreeMap::new(),
    };
    for (name, old_domain) in &old {
        let Some(new_domain) = new.get(name) else {
            continue;
        };
        let kinds = [("commands", "name"), ("events", "name"), ("types", "id")]
            .map(|(list, key)| items(old_domain, new_domain, list, key));
        if kinds.iter().any(|k| !k.is_empty()) {
            diff.domains.insert(name.clone(), kinds);
        }
    }
    diff
}

fn domains(doc: &Value) -> BTreeMap<String, &Value> {
    doc.get("domains")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|d| Some((d.get("domain")?.as_str()?.to_string(), d)))
        .collect()
}

fn items(old: &Value, new: &Value, list: &str, key: &str) -> ItemDiff {
    let index = |domain: &Value| -> BTreeMap<String, Value> {
        domain
            .get(list)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| Some((item.get(key)?.as_str()?.to_string(), item.clone())))
            .collect()
    };
    let (old, new) = (index(old), index(new));
    let names: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let mut diff = ItemDiff::default();
    for name in names {
        match (old.get(name), new.get(name)) {
            (None, Some(_)) => diff.added.push(name.clone()),
            (Some(_), None) => diff.removed.push(name.clone()),
            (Some(a), Some(b)) if a != b => diff.changed.push(name.clone()),
            _ => {}
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn diff_reports_domains_items_and_changed_definitions() {
        let old = json!({ "domains": [
            { "domain": "Page",
              "commands": [ { "name": "navigate", "parameters": [ { "name": "url" } ] },
                            { "name": "crash" } ],
              "events": [ { "name": "loadEventFired" } ],
              "types": [ { "id": "FrameId", "type": "string" } ] },
            { "domain": "Gone" },
            { "domain": "Same", "commands": [ { "name": "x" } ] }
        ]});
        let new = json!({ "domains": [
            { "domain": "Page",
              "commands": [ { "name": "navigate", "parameters": [ { "name": "url" }, { "name": "referrer" } ] },
                            { "name": "close" } ],
              "events": [ { "name": "loadEventFired" } ],
              "types": [ { "id": "FrameId", "type": "string" }, { "id": "Viewport" } ] },
            { "domain": "Fresh" },
            { "domain": "Same", "commands": [ { "name": "x" } ] }
        ]});
        let diff = diff(&old, &new);
        assert_eq!(diff.added_domains, ["Fresh"]);
        assert_eq!(diff.removed_domains, ["Gone"]);
        assert_eq!(diff.domains.keys().collect::<Vec<_>>(), ["Page"]);
        let [commands, events, types] = &diff.domains["Page"];
        assert_eq!(commands.added, ["close"]);
        assert_eq!(commands.removed, ["crash"]);
        assert_eq!(commands.changed, ["navigate"]);
        assert!(events.is_empty());
        assert_eq!(types.added, ["Viewport"]);

        let report = diff.render("browser_protocol.json");
        assert!(report.contains("    + domain Fresh\n"));
        assert!(report.contains("      ~ command navigate\n"));
        assert!(report.contains("      - command crash\n"));
        assert!(super::diff(&old, &old).render("x").contains("no changes"));
    }

    #[test]
    fn integrity_is_checked_against_sha512() {
        let bytes = b"devtools";
        let good = format!(
            "sha512-{}",
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes))
        );
        assert!(verify_integrity(bytes, &good).is_ok());
        assert!(verify_integrity(b"tampered", &good).is_err());
        assert!(verify_integrity(bytes, "sha1-abc").is_err());
    }

    #[test]
    fn extract_finds_both_files() {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, body) in [
            ("package/json/browser_protocol.json", "{\"b\":1}"),
            ("package/json/js_protocol.json", "{\"j\":1}"),
            ("package/README.md", "readme"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, body.as_bytes())
                .unwrap();
        }
        let tarball = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tarball).unwrap();
        let archive = gz.finish().unwrap();

        let [browser, js] = extract(&archive).unwrap();
        assert_eq!((browser.as_str(), js.as_str()), ("{\"b\":1}", "{\"j\":1}"));
    }
}

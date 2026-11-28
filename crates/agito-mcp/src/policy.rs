//! Where the tools may read files from.
//!
//! An agent acts partly on instructions from the pages it visits, so a page could otherwise
//! talk it into uploading the machine's files. `upload_files` reads only inside the roots the
//! MCP client reported for the session, or the server's working directory when the client
//! reports none. `--allow-unrestricted-file-access` lifts the boundary.

use std::path::{Path, PathBuf};

use rmcp::{Peer, RoleServer};

/// The directories reads are confined to: the client's roots, or the working directory.
///
/// Roots are deprecated from protocol version 2026-07-28 (SEP-2577), but remain part of the
/// earlier versions clients negotiate today, which is where clients report them.
#[allow(deprecated)]
pub(crate) async fn read_roots(peer: &Peer<RoleServer>) -> Vec<PathBuf> {
    let reports_roots = peer
        .peer_info()
        .is_some_and(|info| info.capabilities.roots.is_some());
    if reports_roots && let Ok(listed) = peer.list_roots().await {
        let roots: Vec<PathBuf> = listed
            .roots
            .iter()
            .filter_map(|root| local_path(&root.uri))
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    std::env::current_dir().into_iter().collect()
}

/// The local path a `file://` root names.
fn local_path(uri: &str) -> Option<PathBuf> {
    let url = url::Url::parse(uri).ok()?;
    (url.scheme() == "file").then(|| url.to_file_path().ok())?
}

/// `path`, resolved through any symbolic links, if it lies inside one of `roots` (each
/// resolved the same way); otherwise why it may not be read.
pub(crate) fn check_read(path: &str, roots: &[PathBuf]) -> Result<PathBuf, String> {
    let refused = || {
        let listed: Vec<String> = roots.iter().map(|r| r.display().to_string()).collect();
        format!(
            "Reading '{path}' is not allowed. Reads are confined to {}. Start the server with \
             --allow-unrestricted-file-access to lift this.",
            if listed.is_empty() {
                "no directory at all".to_owned()
            } else {
                listed.join(", ")
            }
        )
    };
    let real = std::fs::canonicalize(Path::new(path))
        .map_err(|e| format!("Could not read file '{path}': {e}"))?;
    let inside = roots
        .iter()
        .filter_map(|root| std::fs::canonicalize(root).ok())
        .any(|root| real.starts_with(&root));
    if inside { Ok(real) } else { Err(refused()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_stay_inside_the_roots_links_included() {
        let base = std::env::temp_dir().join(format!("agito-mcp-policy-{}", std::process::id()));
        let inside = base.join("project");
        let outside = base.join("secrets");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(inside.join("photo.png"), b"png").unwrap();
        std::fs::write(outside.join("key"), b"secret").unwrap();
        let roots = vec![inside.clone()];

        let ok = check_read(inside.join("photo.png").to_str().unwrap(), &roots).unwrap();
        assert!(ok.ends_with("photo.png"));
        // Climbing out with .. resolves before the check.
        let climbed = inside.join("../secrets/key");
        let refused = check_read(climbed.to_str().unwrap(), &roots).unwrap_err();
        assert!(refused.starts_with("Reading '"), "{refused}");
        assert!(refused.contains("Reads are confined to"), "{refused}");
        #[cfg(unix)]
        {
            // A link inside the root that points out of it is refused too.
            std::os::unix::fs::symlink(outside.join("key"), inside.join("link")).unwrap();
            let linked = check_read(inside.join("link").to_str().unwrap(), &roots);
            assert!(linked.is_err());
        }
        let missing = check_read(inside.join("nope").to_str().unwrap(), &roots).unwrap_err();
        assert!(missing.starts_with("Could not read file"), "{missing}");
        #[cfg(unix)]
        assert_eq!(
            local_path("file:///home/ada/project"),
            Some(PathBuf::from("/home/ada/project"))
        );
        assert_eq!(local_path("https://example.com/"), None);
        std::fs::remove_dir_all(base).unwrap();
    }
}

/// The directory files the tools write go to, unless `options` name another: one per server
/// process, shared by every session, made on first use.
pub(crate) fn output_dir(options: &crate::ServerOptions) -> PathBuf {
    static DEFAULT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    options.output_dir.clone().unwrap_or_else(|| {
        DEFAULT
            .get_or_init(|| {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or_default();
                std::env::temp_dir().join(format!("agito-mcp-{stamp}-{}", std::process::id()))
            })
            .clone()
    })
}

/// Where a tool writes the file an agent asked for: `requested`, which must stay inside
/// `root`, or a fresh name made of `prefix` and `extension` there. With `unrestricted`, any
/// path goes.
pub(crate) fn resolve_write(
    root: &Path,
    requested: Option<&str>,
    prefix: &str,
    extension: &str,
    unrestricted: bool,
) -> Result<PathBuf, String> {
    let outside = || {
        format!(
            "Paths must stay inside the output directory {}. Start the server with \
             --allow-unrestricted-file-access to lift this.",
            root.display()
        )
    };
    std::fs::create_dir_all(root).map_err(|e| {
        format!(
            "Could not create the output directory {}: {e}",
            root.display()
        )
    })?;
    let requested = requested.map(str::trim).filter(|r| !r.is_empty());
    let Some(requested) = requested else {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        return Ok(root.join(format!("{prefix}-{stamp}.{extension}")));
    };
    let path = Path::new(requested);
    if unrestricted {
        return Ok(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
    }
    if path.is_absolute() || path.has_root() {
        return Err(outside());
    }
    // `..` and links are resolved through the deepest part of the path that exists, which
    // must still be inside the root.
    let candidate = root.join(path);
    let real_root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let mut existing = candidate.as_path();
    let mut rest = Vec::new();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return Err(outside());
        };
        rest.push(name.to_owned());
        existing = parent;
    }
    let mut resolved = std::fs::canonicalize(existing).map_err(|_| outside())?;
    for name in rest.iter().rev() {
        if name == ".." || name == "." {
            return Err(outside());
        }
        resolved.push(name);
    }
    if resolved.starts_with(&real_root) && resolved != real_root {
        Ok(resolved)
    } else {
        Err(outside())
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn writes_stay_inside_the_output_directory() {
        let root = std::env::temp_dir().join(format!("agito-mcp-out-{}", std::process::id()));
        let fresh = resolve_write(&root, None, "trace", "zip", false).unwrap();
        assert!(fresh.starts_with(&root) && fresh.to_string_lossy().ends_with(".zip"));
        let named = resolve_write(&root, Some("runs/checkout.zip"), "trace", "zip", false).unwrap();
        assert!(named.ends_with("runs/checkout.zip"), "{}", named.display());
        assert!(named.starts_with(std::fs::canonicalize(&root).unwrap()));
        for escape in ["../out.zip", "a/../../out.zip", "/etc/out.zip"] {
            let refused = resolve_write(&root, Some(escape), "trace", "zip", false);
            assert!(refused.is_err(), "{escape}");
        }
        #[cfg(unix)]
        {
            let outside =
                std::env::temp_dir().join(format!("agito-mcp-elsewhere-{}", std::process::id()));
            std::fs::create_dir_all(&outside).unwrap();
            std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
            assert!(resolve_write(&root, Some("link/out.zip"), "t", "zip", false).is_err());
            std::fs::remove_dir_all(outside).unwrap();
        }
        assert!(resolve_write(&root, Some("/tmp/anywhere.zip"), "t", "zip", true).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}

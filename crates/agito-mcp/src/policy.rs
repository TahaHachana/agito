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

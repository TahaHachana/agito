use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[test]
fn requested_parses_every_form() {
    let parse = |s: &str| s.parse::<Requested>();
    assert_eq!(
        parse("Stable").unwrap(),
        Requested::Channel(ReleaseChannel::Stable)
    );
    assert_eq!(
        parse("canary").unwrap(),
        Requested::Channel(ReleaseChannel::Canary)
    );
    assert_eq!(
        parse("131.0.6778.85").unwrap(),
        Requested::Version("131.0.6778.85".into())
    );
    assert_eq!(parse("131").unwrap(), Requested::Milestone(131));
    assert_eq!(parse("1368529").unwrap(), Requested::Revision(1368529));
    for bad in ["", "latest", "131.0", "131.0.x.1", "1.2.3.4.5", "-5", "1e6"] {
        assert!(parse(bad).is_err(), "{bad:?} should not parse");
    }
}

#[test]
fn executables_follow_each_archive_layout() {
    assert_eq!(
        Platform::Linux64.executable(),
        Path::new("chrome-linux64").join("chrome")
    );
    assert_eq!(
        Platform::Win64.executable(),
        Path::new("chrome-win64").join("chrome.exe")
    );
    assert!(
        Platform::MacArm64
            .executable()
            .ends_with("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
    );
    assert!(Platform::current().is_ok());
}

fn build(version: &str, revision: &str) -> Build {
    Build {
        version: version.into(),
        revision: revision.into(),
    }
}

#[test]
fn milestone_lookup_names_the_covered_range_on_a_miss() {
    let index = Milestones {
        milestones: HashMap::from([
            ("113".into(), build("113.0.5672.63", "1121455")),
            ("131".into(), build("131.0.6778.85", "1368529")),
        ]),
    };
    assert_eq!(version_for_milestone(&index, 131).unwrap(), "131.0.6778.85");
    let err = version_for_milestone(&index, 90).unwrap_err().to_string();
    assert!(err.contains("113 to 131"), "{err}");
}

#[test]
fn revision_lookup_is_exact_and_names_neighbours() {
    let index = KnownGood {
        versions: vec![
            build("131.0.6778.85", "1368529"),
            build("130.0.6723.58", "1356013"),
            build("132.0.6834.83", "1381561"),
            build("131.0.6778.108", "1368529"),
            build("131.0.6778.0", "1368529"),
        ],
    };
    assert_eq!(
        version_for_revision(&index, 1368529).unwrap(),
        "131.0.6778.108",
        "the newest patch release at a branch point wins"
    );
    assert_eq!(
        version_for_revision(&index, 1356013).unwrap(),
        "130.0.6723.58"
    );
    let err = version_for_revision(&index, 1360000)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("130.0.6723.58 (revision 1356013) and 131.0.6778.108 (revision 1368529)"),
        "{err}"
    );
    let err = version_for_revision(&index, 2000000)
        .unwrap_err()
        .to_string();
    assert!(err.contains("the nearest is 132.0.6834.83"), "{err}");
}

#[test]
fn sandbox_grant_targets_the_restricted_packages_sid() {
    let args = sandbox_acl_arguments(Path::new("C:/b"));
    assert_eq!(args[1], "/grant");
    assert_eq!(args[2], "*S-1-15-2-2:(OI)(CI)(RX)");
}

/// A canned HTTP/1.1 server: path to (status, body), counting requests per path.
struct Server {
    base: String,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Server {
    async fn start(routes: HashMap<String, (u16, Vec<u8>)>) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(HashMap::new()));
        let (routes, counter) = (Arc::new(routes), hits.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let (routes, counter) = (routes.clone(), counter.clone());
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&request);
                    let path = head.split(' ').nth(1).unwrap_or("/").to_string();
                    *counter.lock().unwrap().entry(path.clone()).or_default() += 1;
                    let (status, body) = routes
                        .get(&path)
                        .cloned()
                        .unwrap_or((404, b"not found".to_vec()));
                    let header = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        Server { base, hits }
    }

    fn hits(&self, path: &str) -> usize {
        self.hits.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    fn source(&self) -> Source {
        Source {
            metadata: format!("{}/meta", self.base),
            downloads: format!("{}/dl", self.base),
        }
    }
}

/// A Linux archive laid out as Chrome for Testing lays it out, with an executable and a
/// symlink beside it.
fn linux_archive(with_executable: bool) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    zip.add_directory("chrome-linux64/", opts.unix_permissions(0o755))
        .unwrap();
    if with_executable {
        zip.start_file("chrome-linux64/chrome", opts.unix_permissions(0o755))
            .unwrap();
        zip.write_all(b"#!/bin/sh\n").unwrap();
    }
    zip.start_file("chrome-linux64/resources.pak", opts.unix_permissions(0o644))
        .unwrap();
    zip.write_all(&[7u8; 4096]).unwrap();
    // Windows needs a privilege to create symlinks, so only Unix archives carry one.
    if cfg!(unix) {
        zip.add_symlink("chrome-linux64/link.pak", "resources.pak", opts)
            .unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "agito-install-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

const VERSION: &str = "131.0.6778.85";
const ARCHIVE: &str = "/dl/131.0.6778.85/linux64/chrome-linux64.zip";

fn routes(archive: Vec<u8>) -> HashMap<String, (u16, Vec<u8>)> {
    let channels = serde_json::json!({
        "channels": { "Stable": { "channel": "Stable", "version": VERSION, "revision": "1368529" } }
    });
    let known = serde_json::json!({
        "versions": [ { "version": VERSION, "revision": "1368529" } ]
    });
    HashMap::from([
        (
            "/meta/last-known-good-versions.json".into(),
            (200, channels.to_string().into_bytes()),
        ),
        (
            "/meta/known-good-versions.json".into(),
            (200, known.to_string().into_bytes()),
        ),
        (ARCHIVE.into(), (200, archive)),
    ])
}

fn options(root: &Path, server: &Server, requested: Requested) -> InstallOptions {
    InstallOptions {
        root: root.to_path_buf(),
        requested,
        platform: Platform::Linux64,
        source: server.source(),
    }
}

/// Entries in the cache other than the build directory and the marker.
fn leftovers(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != &format!("chromium-{VERSION}") && n != ".installed.chromium")
        .collect()
}

#[tokio::test]
async fn installs_then_reuses_the_build() {
    let server = Server::start(routes(linux_archive(true))).await;
    let root = scratch("fresh");

    let first = install(&options(&root, &server, Requested::default()))
        .await
        .unwrap();
    let executable = root.join(format!("chromium-{VERSION}/chrome-linux64/chrome"));
    assert_eq!(first.version, VERSION);
    assert_eq!(first.executable, executable);
    assert!(first.downloaded);
    assert_eq!(
        finder::installed(&root, BrowserChannel::Chromium).as_deref(),
        Some(executable.as_path())
    );
    assert!(leftovers(&root).is_empty(), "{:?}", leftovers(&root));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&executable).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the executable keeps its mode");
        let link = executable.with_file_name("link.pak");
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(link).unwrap().len(), 4096);
    }

    // A second install, by revision this time, finds the build and only rewrites the marker.
    std::fs::remove_file(finder::marker_path(&root, BrowserChannel::Chromium)).unwrap();
    let again = install(&options(&root, &server, Requested::Revision(1368529)))
        .await
        .unwrap();
    assert!(!again.downloaded);
    assert_eq!(server.hits(ARCHIVE), 1);
    assert!(finder::installed(&root, BrowserChannel::Chromium).is_some());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn replaces_a_directory_without_the_executable() {
    let server = Server::start(routes(linux_archive(true))).await;
    let root = scratch("incomplete");
    let dir = root.join(format!("chromium-{VERSION}"));
    std::fs::create_dir_all(dir.join("chrome-linux64")).unwrap();
    std::fs::write(dir.join("chrome-linux64/stray"), "x").unwrap();

    let installed = install(&options(&root, &server, Requested::default()))
        .await
        .unwrap();
    assert!(installed.downloaded);
    assert!(installed.executable.is_file());
    assert!(!dir.join("chrome-linux64/stray").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn a_missing_build_says_so_and_leaves_nothing() {
    let server = Server::start(routes(linux_archive(true))).await;
    let root = scratch("missing");
    let err = install(&options(
        &root,
        &server,
        Requested::Version("1.2.3.4".into()),
    ))
    .await
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no 1.2.3.4 build for linux64"),
        "unexpected error: {err}"
    );
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn an_archive_without_the_executable_is_rejected() {
    let server = Server::start(routes(linux_archive(false))).await;
    let root = scratch("noexe");
    let err = install(&options(&root, &server, Requested::default()))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not contain"), "unexpected error: {err}");
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
    assert!(finder::installed(&root, BrowserChannel::Chromium).is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stale_staging_is_swept() {
    let server = Server::start(routes(linux_archive(true))).await;
    let root = scratch("sweep");
    std::fs::create_dir_all(&root).unwrap();
    let old = root.join(format!("{STAGING_PREFIX}chromium-1.0.0.0-1.zip"));
    let fresh = root.join(format!("{STAGING_PREFIX}chromium-1.0.0.0-2"));
    std::fs::write(&old, "partial").unwrap();
    std::fs::create_dir_all(&fresh).unwrap();
    let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();

    install(&options(&root, &server, Requested::default()))
        .await
        .unwrap();
    assert!(!old.exists(), "day-old staging is removed");
    assert!(
        fresh.exists(),
        "a recent one may belong to a running install"
    );
    std::fs::remove_dir_all(root).unwrap();
}

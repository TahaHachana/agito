//! Runs the `agito` binary's screenshot and pdf commands against a real browser.
#![cfg(feature = "integration")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agito-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn agito(args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_agito"))
        .args(args)
        .output()
        .expect("the agito binary runs");
    if !output.status.success() {
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    output
}

fn page(dir: &Path, body: &str) -> String {
    let file = dir.join("page.html");
    std::fs::write(
        &file,
        format!("<!doctype html><style>body{{margin:0}}</style><body>{body}</body>"),
    )
    .unwrap();
    file.to_str().unwrap().to_string()
}

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn screenshot_of_a_local_file_has_the_viewport_size() {
    let dir = scratch("viewport");
    let file = page(&dir, "<div style='height:3000px;background:teal'></div>");
    let out = dir.join("nested/shot.png");

    let run = agito(&[
        "screenshot",
        &file,
        "-o",
        out.to_str().unwrap(),
        "--width",
        "800",
        "--height",
        "600",
    ]);
    assert!(run.status.success());
    assert_eq!(png_size(&std::fs::read(&out).unwrap()), (800, 600));

    let full = dir.join("full.png");
    let run = agito(&[
        "screenshot",
        &file,
        "-o",
        full.to_str().unwrap(),
        "--width",
        "800",
        "--full-page",
    ]);
    assert!(run.status.success());
    assert_eq!(png_size(&std::fs::read(&full).unwrap()), (800, 3000));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn screenshot_format_follows_the_extension() {
    let dir = scratch("jpeg");
    let file = page(&dir, "<h1>hi</h1>");
    let out = dir.join("shot.jpg");
    let run = agito(&[
        "screenshot",
        &file,
        "-o",
        out.to_str().unwrap(),
        "--quality",
        "50",
    ]);
    assert!(run.status.success());
    assert_eq!(&std::fs::read(&out).unwrap()[..3], b"\xff\xd8\xff");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pdf_of_a_data_url() {
    let dir = scratch("pdf");
    let out = dir.join("page.pdf");
    let run = agito(&[
        "pdf",
        "data:text/html,<h1>Report</h1>",
        "-o",
        out.to_str().unwrap(),
        "--landscape",
        "--background",
        "--hide-banners",
    ]);
    assert!(run.status.success());
    assert!(String::from_utf8_lossy(&run.stdout).contains("PDF saved to"));
    assert!(std::fs::read(&out).unwrap().starts_with(b"%PDF-"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unreachable_page_fails_and_writes_nothing() {
    let dir = scratch("unreachable");
    let out = dir.join("shot.png");
    // Port 9 (discard) is closed on CI runners and dev machines alike.
    let run = agito(&[
        "screenshot",
        "http://127.0.0.1:9/",
        "-o",
        out.to_str().unwrap(),
        "--timeout",
        "10",
    ]);
    assert!(!run.status.success());
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("could not load http://127.0.0.1:9/"),
        "{stderr}"
    );
    assert!(!out.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

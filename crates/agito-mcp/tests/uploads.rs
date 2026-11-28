//! `upload_files` and the read boundary: files come only from the client's roots, or the
//! working directory when it reports none. Run with `--features integration`.
#![cfg(feature = "integration")]
#![allow(deprecated)] // Roots: deprecated from 2026-07-28, which clients do not negotiate yet.

mod common;

use std::path::{Path, PathBuf};

use agito_mcp::ServerOptions;
use common::{Harness, page};
use rmcp::model::{ClientCapabilities, ClientInfo, Implementation, ListRootsResult, Root};
use rmcp::service::RequestContext;
use rmcp::{ClientHandler, ErrorData, RoleClient, ServiceExt};
use serde_json::json;

const FILE_INPUT: &str = "<input type=file id=f multiple>";

/// The names the page sees in the file input.
const FILE_NAMES: &str = "[...document.getElementById('f').files].map(f => f.name + ':' + f.size)";

/// A scratch directory, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(under: &Path, name: &str) -> Scratch {
        let dir = under.join(format!("agito-uploads-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn file(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path.to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_roots_reads_stay_in_the_working_directory() {
    let inside = Scratch::new(&std::env::current_dir().unwrap(), "cwd");
    let outside = Scratch::new(&std::env::temp_dir(), "elsewhere");
    let photo = inside.file("photo.png", "four");
    let secret = outside.file("secret.txt", "key");

    let h = Harness::start().await;
    h.ok("navigate", json!({ "url": page(FILE_INPUT) })).await;
    assert_eq!(
        h.ok("upload_files", json!({ "ref": "#f", "paths": [photo] }))
            .await,
        "Uploaded 1 file(s) to #f"
    );
    let names = h
        .call("evaluate", json!({ "expression": FILE_NAMES }))
        .await;
    assert_eq!(
        names.structured_content,
        Some(json!({ "result": ["photo.png:4"] }))
    );

    let refused = h
        .err("upload_files", json!({ "ref": "#f", "paths": [secret] }))
        .await;
    assert!(refused.starts_with("Reading '"), "{refused}");
    assert!(refused.contains("Reads are confined to"), "{refused}");
    h.stop().await;

    let open = Harness::with(ServerOptions {
        allow_unrestricted_file_access: true,
        ..ServerOptions::default()
    })
    .await;
    open.ok("navigate", json!({ "url": page(FILE_INPUT) }))
        .await;
    open.ok("upload_files", json!({ "ref": "#f", "paths": [secret] }))
        .await;
    open.stop().await;
}

/// A client that reports one root.
#[derive(Clone)]
struct RootsClient(String);

impl ClientHandler for RootsClient {
    fn get_info(&self) -> ClientInfo {
        let capabilities: ClientCapabilities =
            serde_json::from_value(json!({ "roots": {} })).unwrap();
        ClientInfo::new(capabilities, Implementation::new("roots-test", "1.0"))
    }

    async fn list_roots(
        &self,
        _: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Ok(ListRootsResult::new(vec![Root::new(self.0.clone())]))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_s_roots_decide_what_may_be_read() {
    let root = Scratch::new(&std::env::temp_dir(), "root");
    let notes = root.file("notes.txt", "hello");
    let in_cwd = Scratch::new(&std::env::current_dir().unwrap(), "not-a-root");
    let local = in_cwd.file("local.txt", "x");
    let uri = url::Url::from_directory_path(&root.0).unwrap().to_string();

    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(async move {
        let (reader, writer) = tokio::io::split(server_io);
        agito_mcp::serve(ServerOptions::default(), reader, writer)
            .await
            .unwrap();
    });
    let client = RootsClient(uri).serve(client_io).await.unwrap();
    let call = |tool: &'static str, args: serde_json::Value| {
        let serde_json::Value::Object(args) = args else {
            panic!()
        };
        client.call_tool(rmcp::model::CallToolRequestParams::new(tool).with_arguments(args))
    };
    call("navigate", json!({ "url": page(FILE_INPUT) }))
        .await
        .unwrap();
    let ok = call("upload_files", json!({ "ref": "#f", "paths": [notes] }))
        .await
        .unwrap();
    assert_ne!(ok.is_error, Some(true), "{ok:?}");
    // The working directory is not a root once the client names its own.
    let refused = call("upload_files", json!({ "ref": "#f", "paths": [local] }))
        .await
        .unwrap();
    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    client.cancel().await.unwrap();
    server.await.unwrap();
}

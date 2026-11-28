//! `agito mcp` as an MCP client launches it: a child process spoken to over stdio. Run with
//! `--features integration`.
#![cfg(feature = "integration")]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

/// A minimal MCP client: one exchange at a time, as clients do.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn launch() -> Client {
        Client::launch_with(&[])
    }

    fn launch_with(args: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agito"))
            .arg("mcp")
            .args(args)
            .env_remove("AGITO_MCP_CAPS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("agito mcp starts");
        Client {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next_id: 0,
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{message}").unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("a response");
        let response: Value = serde_json::from_str(&line).expect("only JSON-RPC on stdout");
        assert_eq!(response["id"], id, "{response}");
        response["result"].clone()
    }

    /// Calls a tool, which must succeed, and returns its result.
    fn call(&mut self, name: &str, args: Value) -> Value {
        let result = self.request("tools/call", json!({ "name": name, "arguments": args }));
        assert_ne!(result["isError"], true, "{name}: {result}");
        result
    }

    /// Closes stdin, which ends the session, and waits for the server to exit.
    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        self.child.wait().expect("agito mcp exits")
    }
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap_or_default()
}

#[test]
fn a_client_drives_a_page_through_agito_mcp() {
    let mut client = Client::launch();
    let init = client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }),
    );
    assert_eq!(init["serverInfo"]["name"], "agito");
    assert!(init["capabilities"]["tools"].is_object(), "{init}");
    client.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let tools = client.request("tools/list", json!({}));
    assert_eq!(tools["tools"].as_array().unwrap().len(), 33, "{tools}");

    let navigated = client.call(
        "navigate",
        json!({ "url": "data:text/html,<title>Hello</title><button onclick=\"document.title='Clicked'\">Go</button>" }),
    );
    assert!(text(&navigated).ends_with(" | Hello"), "{navigated}");
    let tree = client.call("snapshot", json!({}));
    assert!(text(&tree).contains(r#"button "Go" [ref=e2]"#), "{tree}");
    client.call("click", json!({ "ref": "e2" }));
    let title = client.call("evaluate", json!({ "expression": "document.title" }));
    assert_eq!(title["structuredContent"], json!({ "result": "Clicked" }));

    // The server closes its browser on the way out.
    let status = client.finish();
    assert!(status.success(), "{status}");
}

#[test]
fn caps_add_tool_groups_and_unknown_ones_are_refused() {
    let mut client = Client::launch_with(&["--caps", "coordinates", "--caps=routing"]);
    client.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }),
    );
    client.send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let tools = client.request("tools/list", json!({}));
    assert_eq!(
        tools["tools"].as_array().unwrap().len(),
        33 + 6 + 5,
        "{tools}"
    );
    assert!(client.finish().success());

    let refused = Command::new(env!("CARGO_BIN_EXE_agito"))
        .args(["mcp", "--caps", "coordinates,teleport"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("--caps: unknown tool group 'teleport'"),
        "{stderr}"
    );
}

//! Protocol tests over `lsp_server::Connection::memory()` (plan §5.7):
//! full sessions covering lifecycle in both encodings, document sync,
//! and debounced publishDiagnostics.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};

/// A test client driving a real server on a background thread.
struct Client {
    connection: Connection,
    server: Option<std::thread::JoinHandle<Result<(), morog_lsp::Error>>>,
    next_id: i32,
}

const TIMEOUT: Duration = Duration::from_secs(30);

impl Client {
    /// Start a server and run `initialize` with the given client encodings.
    fn start(encodings: &[&str], init_options: Value) -> Self {
        let (server_side, client_side) = Connection::memory();
        let server = std::thread::spawn(move || morog_lsp::run(server_side));
        let mut client = Self {
            connection: client_side,
            server: Some(server),
            next_id: 0,
        };
        let response = client.request(
            "initialize",
            json!({
                "capabilities": { "general": { "positionEncodings": encodings } },
                "initializationOptions": init_options,
            }),
        );
        let encoding = &response["capabilities"]["positionEncoding"];
        let expected = if encodings.contains(&"utf-8") {
            "utf-8"
        } else {
            "utf-16"
        };
        assert_eq!(encoding, expected, "negotiated encoding");
        client.notify("initialized", json!({}));
        client
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.connection
            .sender
            .send(Message::Request(Request::new(
                id.clone(),
                method.to_owned(),
                params,
            )))
            .unwrap();
        loop {
            match self.recv() {
                Message::Response(response) => {
                    assert_eq!(response.id, id);
                    return response
                        .response_result
                        .unwrap_or_else(|e| panic!("request `{method}` failed: {e:?}"));
                }
                // Skip server-initiated notifications (diagnostics etc.).
                Message::Notification(_) => continue,
                Message::Request(_) => continue,
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(Message::Notification(Notification::new(
                method.to_owned(),
                params,
            )))
            .unwrap();
    }

    fn recv(&self) -> Message {
        self.connection
            .receiver
            .recv_timeout(TIMEOUT)
            .expect("server went quiet")
    }

    /// Wait for the next publishDiagnostics for `uri`.
    fn recv_diagnostics(&self, uri: &str) -> Value {
        loop {
            if let Message::Notification(notification) = self.recv()
                && notification.method == "textDocument/publishDiagnostics"
                && notification.params["uri"] == uri
            {
                return notification.params;
            }
        }
    }

    fn open(&self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri, "languageId": "markdown",
                    "version": version, "text": text,
                }
            }),
        );
    }

    fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        self.server
            .take()
            .unwrap()
            .join()
            .expect("server thread panicked")
            .expect("server returned an error");
    }
}

fn codes(diags: &Value) -> Vec<String> {
    diags["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[test]
fn lifecycle_utf8() {
    let client = Client::start(&["utf-8", "utf-16"], json!({ "vaultRoot": "/nonexistent" }));
    client.shutdown();
}

#[test]
fn lifecycle_utf16_fallback() {
    let client = Client::start(&[], json!({ "vaultRoot": "/nonexistent" }));
    client.shutdown();
}

#[test]
fn unknown_requests_get_method_not_found() {
    let mut client = Client::start(&["utf-8"], json!({ "vaultRoot": "/nonexistent" }));
    client.next_id += 1;
    let id = RequestId::from(client.next_id);
    client
        .connection
        .sender
        .send(Message::Request(Request::new(
            id.clone(),
            "textDocument/typeDefinition".to_owned(),
            json!({}),
        )))
        .unwrap();
    loop {
        if let Message::Response(response) = client.recv() {
            assert_eq!(response.id, id);
            let error = response
                .response_result
                .expect_err("expected an error response");
            assert_eq!(error.code, lsp_server::ErrorCode::MethodNotFound as i32);
            break;
        }
    }
    client.shutdown();
}

#[test]
fn diagnostics_on_open_change_and_close() {
    let client = Client::start(&["utf-8"], json!({ "vaultRoot": "/vault" }));
    let uri = "file:///vault/topic.md";

    // A topic with one uncited block.
    client.open(
        uri,
        1,
        "---\nid: sla\nkind: topic\n---\n\nAn uncited paragraph.\n",
    );
    let diags = client.recv_diagnostics(uri);
    assert_eq!(diags["version"], 1);
    assert_eq!(codes(&diags), ["sta::uncited-block"]);
    let range = &diags["diagnostics"][0]["range"];
    assert_eq!(range["start"]["line"], 5);
    assert_eq!(range["start"]["character"], 0);

    // Tag the block #mine: replace the trailing "." with ". #mine" — the
    // debounce coalesces into one publish for version 2.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 5, "character": 21 },
                    "end": { "line": 5, "character": 21 },
                },
                "text": " #mine",
            }],
        }),
    );
    let diags = client.recv_diagnostics(uri);
    assert_eq!(diags["version"], 2);
    assert_eq!(codes(&diags), Vec::<String>::new());

    // Closing clears.
    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );
    let diags = client.recv_diagnostics(uri);
    assert_eq!(codes(&diags), Vec::<String>::new());
    assert_eq!(diags["version"], Value::Null);

    client.shutdown();
}

#[test]
fn cross_file_diagnostics_follow_edits_across_files() {
    let client = Client::start(&["utf-8"], json!({ "vaultRoot": "/vault" }));
    let arg_uri = "file:///vault/arg.md";
    let topic_uri = "file:///vault/topic.md";

    // An argument whose two relations dangle (plus one is positional).
    client.open(
        arg_uri,
        1,
        "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: supports }\n  - { to: sla#b2, stance: opposes }\n---\n",
    );
    let diags = client.recv_diagnostics(arg_uri);
    assert_eq!(
        codes(&diags),
        ["sta::relation-target", "sta::positional-address"]
    );

    // Opening the topic resolves the first relation; the second now names a
    // missing (positional) anchor on a known note.
    client.open(
        topic_uri,
        1,
        "---\nid: sla\nkind: topic\n---\n\nCited [@w2004]. #anchor e1\n",
    );
    let diags = client.recv_diagnostics(arg_uri);
    assert_eq!(codes(&diags), ["sta::positional-address"]);

    // Fixing the target to the declared anchor clears the argument.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": arg_uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 6, "character": 14 },
                    "end": { "line": 6, "character": 16 },
                },
                "text": "e1",
            }],
        }),
    );
    let diags = client.recv_diagnostics(arg_uri);
    assert_eq!(diags["version"], 2);
    assert_eq!(codes(&diags), Vec::<String>::new());

    client.shutdown();
}

#[test]
fn utf16_positions_in_diagnostics() {
    let client = Client::start(&[], json!({ "vaultRoot": "/vault" }));
    let uri = "file:///vault/topic.md";

    // The uncited block sits after a 🦀 (2 UTF-16 units, 4 UTF-8 bytes) on
    // the same line, so the reported start column differs per encoding.
    client.open(
        uri,
        1,
        "---\nid: sla\nkind: topic\n---\n\n🦀 uncited after a crab.\n",
    );
    let diags = client.recv_diagnostics(uri);
    assert_eq!(codes(&diags), ["sta::uncited-block"]);
    let range = &diags["diagnostics"][0]["range"];
    assert_eq!(range["start"]["character"], 0);
    // End of block: "🦀 uncited after a crab." = 2 + 22 UTF-16 units.
    assert_eq!(range["end"]["character"], 24);

    client.shutdown();
}

#[test]
fn navigation_requests_over_open_documents() {
    let mut client = Client::start(&["utf-8"], json!({ "vaultRoot": "/vault" }));
    let sla_uri = "file:///vault/sla.md";
    let arg_uri = "file:///vault/arg.md";

    client.open(
        sla_uri,
        1,
        "---\nid: sla\nkind: topic\ntitle: \"Specific leaf area\"\n---\n\n# Evidence\n\nSLA anchors the spectrum [@wright2004]. #anchor e1\n",
    );
    client.open(
        arg_uri,
        1,
        "---\nid: arg\nkind: argument\ntitle: \"A\"\nthesis: \"It is allocation.\"\nrelations:\n  - { to: sla#e1, stance: supports, op: analyze }\n  - { to: sla, stance: qualifies }\n---\n",
    );

    // hover on the relation target `sla#e1` (line 6, inside "sla#e1").
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": arg_uri },
            "position": { "line": 6, "character": 11 },
        }),
    );
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(
        value.contains("**topic** `sla` — Specific leaf area"),
        "{value}"
    );
    assert!(value.contains("SLA anchors the spectrum"), "{value}");

    // definition of the same target: the anchored block in sla.md.
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": arg_uri },
            "position": { "line": 6, "character": 11 },
        }),
    );
    assert_eq!(definition["uri"], sla_uri);
    assert_eq!(definition["range"]["start"]["line"], 8);

    // references on the topic's id declaration: both argument relations.
    let references = client.request(
        "textDocument/references",
        json!({
            "textDocument": { "uri": sla_uri },
            "position": { "line": 1, "character": 5 },
            "context": { "includeDeclaration": false },
        }),
    );
    let refs = references.as_array().unwrap();
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().all(|r| r["uri"] == arg_uri));

    // documentSymbol on the topic: frontmatter + heading + evidence block.
    let symbols = client.request(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": sla_uri } }),
    );
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["topic sla", "Evidence"]);
    assert_eq!(symbols[1]["children"][0]["name"], "@wright2004");

    // workspace/symbol sees both notes, kind-prefixed.
    let symbols = client.request("workspace/symbol", json!({ "query": "" }));
    let names: Vec<&str> = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["argument arg", "topic sla"]);

    client.shutdown();
}

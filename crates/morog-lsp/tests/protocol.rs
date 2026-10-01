//! Protocol tests over `lsp_server::Connection::memory()` (plan §5.7):
//! full sessions covering lifecycle in both encodings, document sync,
//! debounced publishDiagnostics, and the L3/L4 request surface.

mod support;

use lsp_server::{Message, Request, RequestId};
use serde_json::{Value, json};
use support::{Client, codes};

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

/// Completion labels at a position, in response order.
fn completion_labels(client: &mut Client, uri: &str, line: u32, character: u32) -> Vec<String> {
    client
        .request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn completion_over_open_documents() {
    let mut client = Client::start(&["utf-8"], json!({ "vaultRoot": "/vault" }));
    let sla_uri = "file:///vault/sla.md";
    let arg_uri = "file:///vault/arg.md";
    client.open(
        sla_uri,
        1,
        "---\nid: sla\nkind: topic\ntitle: \"Specific leaf area\"\n---\n\nEvidence [@w]. #anchor e1\n",
    );
    client.open(
        arg_uri,
        1,
        "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: , stance: su }\n---\n",
    );

    // `to: |` offers topics and arguments (sources never appear).
    let labels = completion_labels(&mut client, arg_uri, 5, 10);
    assert_eq!(labels, ["arg", "sla"]);

    // `stance: su|` narrows the closed stance vocabulary.
    let labels = completion_labels(&mut client, arg_uri, 5, 22);
    assert_eq!(labels, ["supports"]);

    // The completion item carries a text edit replacing the typed partial.
    let completion = client.request(
        "textDocument/completion",
        json!({
            "textDocument": { "uri": arg_uri },
            "position": { "line": 5, "character": 22 },
        }),
    );
    let edit = &completion[0]["textEdit"];
    assert_eq!(edit["newText"], "supports");
    assert_eq!(edit["range"]["start"]["character"], 20);
    assert_eq!(edit["range"]["end"]["character"], 22);

    // `#m` in prose offers the tag vocabulary, `mine` first.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": sla_uri, "version": 2 },
            "contentChanges": [{
                "range": {
                    "start": { "line": 6, "character": 0 },
                    "end": { "line": 6, "character": 0 },
                },
                "text": "An idea. #m\n",
            }],
        }),
    );
    let labels = completion_labels(&mut client, sla_uri, 6, 11);
    assert_eq!(labels[0], "mine");
    assert!(labels.contains(&"media".to_owned()));

    client.shutdown();
}

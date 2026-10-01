//! Integration over the nine-note fixture vault (plan §5.7): cold start
//! from a prebuilt index with no reparse, navigation across unopened
//! files, diagnostics from cold facts, and the save → indexer → analysis
//! round trip. Uses a temp `XDG_CACHE_HOME` so the real cache stays clean.
//!
//! One `#[test]` only: the cache location is process-global state.

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use support::{Client, codes};

/// Recursively copy the `*.md` fixture tree.
fn copy_vault(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_vault(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn uri(path: &Path) -> String {
    lsp_types::Url::from_file_path(path).unwrap().to_string()
}

/// (line, character) of `needle` in `text`, 0-based, ASCII fixture text.
fn position_of(text: &str, needle: &str) -> (usize, usize) {
    let offset = text.find(needle).expect("needle present");
    let line = text[..offset].matches('\n').count();
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    (line, offset - line_start)
}

fn symbol_names(client: &mut Client) -> Vec<String> {
    client
        .request("workspace/symbol", json!({ "query": "" }))
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn cold_start_from_index_over_the_fixture_vault() {
    // Isolate the index cache; single-test binary, so this is safe.
    let scratch = std::env::temp_dir().join(format!(
        "morog-lsp-cold-start-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(scratch.join("cache")).unwrap();
    unsafe { std::env::set_var("XDG_CACHE_HOME", scratch.join("cache")) };

    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../morog/tests/fixtures/vault")
        .canonicalize()
        .unwrap();
    let vault = scratch.join("vault");
    copy_vault(&fixtures, &vault);
    let vault = vault.canonicalize().unwrap();

    // A prior `morog index` run filled the cache; the server must cold
    // start from it with a table scan, not a reparse (plan §5.4).
    let db = morog_index::cache_db_path(&vault);
    morog_index::index_vault(&vault, &db, true).unwrap();

    let mut client = Client::start(&["utf-8"], json!({ "vaultRoot": vault.to_str().unwrap() }));

    // workspace/symbol works before any document is opened. weak-arg is
    // declared twice, but a duplicate id keeps only one `notes` row in the
    // index (`store_file_facts`), so the cold start sees six notes;
    // bad-kind and plain carry no id at all.
    let names = symbol_names(&mut client);
    assert_eq!(
        names,
        [
            "argument broken-targets",
            "topic leaf-mass-ratio",
            "source paszke_pytorch_2019",
            "topic specific-leaf-area",
            "argument two-shape-classes",
            "argument weak-arg",
        ]
    );

    // Goto definition across two unopened files: the argument's relation
    // target `specific-leaf-area#evidence-1` resolves to the topic block.
    let arg_path = vault.join("arguments/two-shape-classes.md");
    let arg_text = std::fs::read_to_string(&arg_path).unwrap();
    let (line, character) = position_of(&arg_text, "specific-leaf-area#evidence-1");
    let definition = client.request(
        "textDocument/definition",
        json!({
            "textDocument": { "uri": uri(&arg_path) },
            "position": { "line": line, "character": character + 2 },
        }),
    );
    let topic_path = vault.join("topics/specific-leaf-area.md");
    assert_eq!(definition["uri"], uri(&topic_path));
    let topic_text = std::fs::read_to_string(&topic_path).unwrap();
    let (block_line, _) = position_of(&topic_text, "SLA is projected");
    assert_eq!(definition["range"]["start"]["line"], block_line);

    // Hover across unopened files reads block text lazily from disk.
    let hover = client.request(
        "textDocument/hover",
        json!({
            "textDocument": { "uri": uri(&arg_path) },
            "position": { "line": line, "character": character + 2 },
        }),
    );
    let value = hover["contents"]["value"].as_str().unwrap();
    assert!(value.contains("**topic** `specific-leaf-area`"), "{value}");
    assert!(value.contains("SLA is projected"), "{value}");

    // Opening the broken argument yields the vault-level diagnostics
    // computed against cold-start facts.
    let broken_path = vault.join("arguments/broken-targets.md");
    let broken_uri = uri(&broken_path);
    client.open(
        &broken_uri,
        1,
        &std::fs::read_to_string(&broken_path).unwrap(),
    );
    let diags = client.recv_diagnostics(&broken_uri);
    assert_eq!(
        codes(&diags),
        ["sta::relation-target", "sta::positional-address"]
    );

    // Save → indexer → analysis: a new topic written to disk appears in
    // the symbol index after the didSave-triggered reconcile.
    let new_path = vault.join("topics/new-topic.md");
    std::fs::write(
        &new_path,
        "---\nid: new-topic\nkind: topic\ntitle: \"New topic\"\n---\n\nFresh [@paszke_pytorch_2019]. #anchor n1\n",
    )
    .unwrap();
    client.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": uri(&new_path) } }),
    );
    let deadline = Instant::now() + support::TIMEOUT;
    loop {
        let names = symbol_names(&mut client);
        if names.iter().any(|n| n == "topic new-topic") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "indexer never picked up the save"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    client.shutdown();
    let _ = std::fs::remove_dir_all(&scratch);
}

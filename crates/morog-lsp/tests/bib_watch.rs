//! Live bibrat freshness (plan §5.4): cold-start tag completion straight
//! from `TAGS.yml`, and the watched-files → store re-walk → bibliography
//! refresh round trip over a temp bibrat fixture tree. Uses a temp
//! `XDG_CACHE_HOME` so the real index cache stays clean.
//!
//! One `#[test]` only: the cache location is process-global state.

mod support;

use std::path::Path;

use serde_json::json;
use support::{Client, codes};

fn uri(path: &Path) -> String {
    lsp_types::Url::from_file_path(path).unwrap().to_string()
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
fn bibrat_watch_refreshes_bibliography_and_tags() {
    // Isolate the index cache; single-test binary, so this is safe.
    let scratch = std::env::temp_dir().join(format!(
        "morog-lsp-bib-watch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(scratch.join("cache")).unwrap();
    unsafe { std::env::set_var("XDG_CACHE_HOME", scratch.join("cache")) };

    // A bibrat fixture store: one category, one entry, a root vocabulary.
    let bib_root = scratch.join("bibrat");
    std::fs::create_dir_all(bib_root.join("papers/wright2004")).unwrap();
    std::fs::write(
        bib_root.join("papers/wright2004/citation.bib"),
        "@article{wright2004, title={The worldwide leaf economics spectrum}, year={2004}}\n",
    )
    .unwrap();
    std::fs::write(bib_root.join("TAGS.yml"), "tags:\n  plastix-vocab: null\n").unwrap();
    let bib_root = bib_root.canonicalize().unwrap();

    let vault = scratch.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let vault = vault.canonicalize().unwrap();

    let mut client = Client::start(
        &["utf-8"],
        json!({
            "vaultRoot": vault.to_str().unwrap(),
            "bibratRoot": bib_root.to_str().unwrap(),
        }),
    );

    // Cold-start tags: the `TAGS.yml` vocabulary completes from the first
    // keystroke — the server reads it at startup rather than waiting for
    // the first indexer reconcile to stream it.
    let topic_uri = uri(&vault.join("topic.md"));
    client.open(
        &topic_uri,
        1,
        "---\nid: t\nkind: topic\n---\n\nIdea [@wright2004]. #pl\n",
    );
    let labels = completion_labels(&mut client, &topic_uri, 5, 23);
    assert!(labels.contains(&"plastix-vocab".to_owned()), "{labels:?}");

    // A note citing a key the store does not have yet: once the startup
    // reconcile lands the bibliography, the citekey lint fires.
    let cite_uri = uri(&vault.join("cite.md"));
    client.open(
        &cite_uri,
        1,
        "---\nid: c\nkind: topic\n---\n\nNew [@fresh2026]. #mine\n",
    );
    loop {
        let diags = client.recv_diagnostics(&cite_uri);
        if codes(&diags).contains(&"sta::unknown-citekey".to_owned()) {
            break;
        }
    }

    // Grow the store on disk (a new entry directory and a bigger
    // vocabulary) and deliver the watched-files notification: the server
    // re-walks the store, refreshes `bib_entries` through the embedded
    // indexer, and the open file's lint clears.
    std::fs::create_dir_all(bib_root.join("papers/fresh2026")).unwrap();
    std::fs::write(
        bib_root.join("papers/fresh2026/citation.bib"),
        "@article{fresh2026, title={Fresh findings}, year={2026}}\n",
    )
    .unwrap();
    std::fs::write(
        bib_root.join("TAGS.yml"),
        "tags:\n  plastix-vocab: null\n  fresh-vocab: null\n",
    )
    .unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({
            "changes": [
                { "uri": uri(&bib_root.join("papers/fresh2026/citation.bib")), "type": 1 },
                { "uri": uri(&bib_root.join("TAGS.yml")), "type": 2 },
            ],
        }),
    );
    loop {
        let diags = client.recv_diagnostics(&cite_uri);
        if codes(&diags).is_empty() {
            break;
        }
    }

    // The refreshed bibliography drives completion: the new key is offered
    // inside `[@fr…` …
    let labels = completion_labels(&mut client, &cite_uri, 5, 8);
    assert!(labels.contains(&"fresh2026".to_owned()), "{labels:?}");

    // … and the re-read vocabulary extends tag completion.
    let labels = completion_labels(&mut client, &topic_uri, 5, 21);
    assert!(labels.contains(&"fresh-vocab".to_owned()), "{labels:?}");

    client.shutdown();
    let _ = std::fs::remove_dir_all(&scratch);
}

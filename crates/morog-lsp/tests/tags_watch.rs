//! Live `[tags]` reconfiguration (plan §10 T1): the resolved tag table
//! loads at startup (vault-local `.morog.toml` over the morog and morg XDG
//! configs), drives completion, semantic tokens, and the
//! `tag::shape-mismatch` diagnostic, and a watched-files change to any
//! config layer rebuilds the table and reparses open buffers. Uses a temp
//! `XDG_CACHE_HOME`/`XDG_CONFIG_HOME` so the real caches and configs stay
//! clean.
//!
//! One `#[test]` only: the cache and config locations are process-global
//! state (the `bib_watch` approach).

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

/// The token-type indices of a `semanticTokens/full` response, decoded from
/// the delta encoding (every 5th datum, offset 3).
fn semantic_token_types(client: &mut Client, uri: &str) -> Vec<u64> {
    client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    )["data"]
        .as_array()
        .unwrap()
        .chunks(5)
        .map(|token| token[3].as_u64().unwrap())
        .collect()
}

/// `property` in the advertised legend — named-group custom tag fields.
const TOKEN_CUSTOM_FIELD: u64 = 6;

#[test]
fn tags_config_watch_rebuilds_the_table() {
    // Isolate the index cache and the config homes; single-test binary, so
    // this is safe.
    let scratch = std::env::temp_dir().join(format!(
        "morog-lsp-tags-watch-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(scratch.join("cache")).unwrap();
    std::fs::create_dir_all(scratch.join("config")).unwrap();
    unsafe {
        std::env::set_var("XDG_CACHE_HOME", scratch.join("cache"));
        std::env::set_var("XDG_CONFIG_HOME", scratch.join("config"));
    }

    // A vault whose `.morog.toml` declares #book with named groups.
    let vault = scratch.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let vault = vault.canonicalize().unwrap();
    let vault_config = vault.join(".morog.toml");
    std::fs::write(
        &vault_config,
        "[tags.book]\npattern = '\"(?<title>[^\"]+)\" by (?<author>.+)'\n",
    )
    .unwrap();

    let mut client = Client::start(&["utf-8"], json!({ "vaultRoot": vault.to_str().unwrap() }));

    // One matching occurrence (named-group sub-spans) and one mismatch (the
    // lint): the table resolved at startup drives both.
    let note_uri = uri(&vault.join("note.md"));
    client.open(
        &note_uri,
        1,
        "---\nid: n\nkind: topic\n---\n\nIdea [@k]. #book \"Dune\" by Frank Herbert\n\nOops. #book junk #mine\n",
    );
    loop {
        let diags = client.recv_diagnostics(&note_uri);
        if codes(&diags).contains(&"tag::shape-mismatch".to_owned()) {
            break;
        }
    }

    // The declared name joins keyword completion in the Tag context …
    let labels = completion_labels(&mut client, &note_uri, 5, 12);
    assert!(labels.contains(&"book".to_owned()), "{labels:?}");

    // … and the matched occurrence's fields get `property` tokens.
    let types = semantic_token_types(&mut client, &note_uri);
    assert_eq!(
        types
            .iter()
            .filter(|&&token| token == TOKEN_CUSTOM_FIELD)
            .count(),
        2, // title and author, matched occurrence only
        "{types:?}"
    );

    // Declaring a tag in the morog XDG config layer and delivering the
    // watched-files notification extends the merged table live.
    let morog_config = scratch.join("config/morog/config.toml");
    std::fs::create_dir_all(morog_config.parent().unwrap()).unwrap();
    std::fs::write(&morog_config, "[tags.pace]\nkind = \"duration\"\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": uri(&morog_config), "type": 1 }] }),
    );
    // The main loop handles the notification before the next request, so
    // the rebuilt table is visible immediately.
    let labels = completion_labels(&mut client, &note_uri, 5, 12);
    assert!(labels.contains(&"pace".to_owned()), "{labels:?}");
    assert!(labels.contains(&"book".to_owned()), "{labels:?}");

    // Loosening the vault-local pattern re-resolves the table, reparses the
    // open buffer, and clears the mismatch lint.
    std::fs::write(&vault_config, "[tags.book]\npattern = '(?<what>.+)'\n").unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": uri(&vault_config), "type": 2 }] }),
    );
    loop {
        let diags = client.recv_diagnostics(&note_uri);
        if codes(&diags).is_empty() {
            break;
        }
    }

    client.shutdown();
    let _ = std::fs::remove_dir_all(&scratch);
}

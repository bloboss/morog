//! Manual performance measurements against the plan §5.7 budgets, over the
//! generated 5M-word vault (`cargo run --release -p morog-bench -- gen`):
//!
//! ```sh
//! cargo test --release -p morog-lsp --test bench -- --ignored --nocapture
//! ```
//!
//! - cold start (index table scan → analysis, no reparse): budget < 1 s
//! - keystroke → publishDiagnostics, net of the 150 ms debounce: < 30 ms
//! - completion round trip: < 20 ms

mod support;

use std::path::Path;
use std::time::{Duration, Instant};

use lsp_server::Message;
use serde_json::json;
use support::Client;

#[test]
#[ignore = "manual benchmark; needs bench/vault and --release"]
fn bench_cold_start_and_keystroke() {
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/vault");
    if !vault.is_dir() {
        eprintln!("bench/vault missing; run: cargo run --release -p morog-bench -- gen");
        return;
    }
    let vault = vault.canonicalize().unwrap();

    // Bring the cache up to date, as `morog index` would have.
    let db = morog_index::cache_db_path(&vault);
    let t = Instant::now();
    let stats = morog_index::index_vault(&vault, &db, false).unwrap();
    eprintln!(
        "index reconcile before start: {:?} ({} scanned, {} indexed)",
        t.elapsed(),
        stats.scanned,
        stats.indexed
    );

    // Cold start: the synchronous table-scan load runs between the
    // initialize response and the main loop, so the first request's
    // round trip bounds it from above.
    let t = Instant::now();
    let mut client = Client::start(&["utf-8"], json!({ "vaultRoot": vault.to_str().unwrap() }));
    let t_init = t.elapsed();
    let t = Instant::now();
    let symbols = client.request("workspace/symbol", json!({ "query": "" }));
    let t_cold = t.elapsed();
    eprintln!(
        "initialize round trip: {t_init:?}; cold load + workspace/symbol: {t_cold:?} \
         ({} symbols)  [budget: cold start < 1 s]",
        symbols.as_array().map_or(0, Vec::len)
    );

    // Let the background reconcile finish so it doesn't pollute keystroke
    // timings.
    loop {
        if let Message::Notification(n) = client.recv()
            && n.method == "$/progress"
            && n.params["value"]["kind"] == "end"
        {
            eprintln!(
                "reconcile: {}",
                n.params["value"]["message"].as_str().unwrap_or("?")
            );
            break;
        }
    }

    // Keystroke → diagnostics on a bench topic file (topics are sharded
    // into subdirectories: topics/000/x.md).
    let topic = std::fs::read_dir(vault.join("topics"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .flat_map(|p| {
            if p.is_dir() {
                std::fs::read_dir(p)
                    .map(|dir| dir.filter_map(Result::ok).map(|e| e.path()).collect())
                    .unwrap_or_default()
            } else {
                vec![p]
            }
        })
        .find(|p| p.extension().is_some_and(|x| x == "md"))
        .expect("bench vault has topics");
    let text = std::fs::read_to_string(&topic).unwrap();
    let uri = lsp_types::Url::from_file_path(&topic).unwrap().to_string();
    let lines = text.matches('\n').count();
    client.open(&uri, 1, &text);
    client.recv_diagnostics(&uri);

    let debounce = Duration::from_millis(150);
    let mut samples = Vec::new();
    for version in 2..=12 {
        let t = Instant::now();
        client.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri, "version": version },
                "contentChanges": [{
                    "range": {
                        "start": { "line": lines, "character": 0 },
                        "end": { "line": lines, "character": 0 },
                    },
                    "text": "x",
                }],
            }),
        );
        client.recv_diagnostics(&uri);
        samples.push(t.elapsed().saturating_sub(debounce));
    }
    samples.sort_unstable();
    eprintln!(
        "keystroke -> diagnostics net of {debounce:?} debounce over {} samples: \
         min {:?}, median {:?}, max {:?}  [budget < 30 ms]  (file: {}, {} bytes)",
        samples.len(),
        samples.first().unwrap(),
        samples[samples.len() / 2],
        samples.last().unwrap(),
        topic.file_name().unwrap().to_string_lossy(),
        text.len(),
    );

    // Completion round trip (plan §5.7: < 20 ms). Target completion is the
    // expensive path: it fuzzy-scans every note in the vault. The cursor
    // goes at the end of the appended line; typing `(id:x` first makes the
    // line a link-target context.
    let lines = text.matches('\n').count();
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 100 },
            "contentChanges": [{
                "range": {
                    "start": { "line": lines, "character": 0 },
                    "end": { "line": lines, "character": 0 },
                },
                "text": "see [x](id:a",
            }],
        }),
    );
    let mut samples = Vec::new();
    let mut items = 0;
    for _ in 0..10 {
        let t = Instant::now();
        let completion = client.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": lines, "character": 12 },
            }),
        );
        samples.push(t.elapsed());
        items = completion.as_array().map_or(0, Vec::len);
    }
    samples.sort_unstable();
    eprintln!(
        "completion round trip over {} samples: min {:?}, median {:?}, max {:?} \
         ({items} items)  [budget < 20 ms]",
        samples.len(),
        samples.first().unwrap(),
        samples[samples.len() / 2],
        samples.last().unwrap(),
    );

    client.shutdown();
}

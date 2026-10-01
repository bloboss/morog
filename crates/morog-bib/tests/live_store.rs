//! Smoke test against the user's real bibrat store, when one exists at
//! `~/.local/share/bibrat`. Read-only; skips cleanly on machines without a
//! store. Run with `--nocapture` to see the per-category census.

use std::collections::BTreeMap;
use std::path::PathBuf;

use morog_bib::{BibStore, BibratStore};

#[test]
fn live_store_walks_clean() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("HOME unset; skipping live-store smoke test");
        return;
    };
    let root = home.join(".local").join("share").join("bibrat");
    if !root.is_dir() {
        eprintln!("no live bibrat store at {}; skipping", root.display());
        return;
    }

    let t0 = std::time::Instant::now();
    let store = BibratStore::open(&root).expect("live store walks without errors");
    let wall = t0.elapsed();

    let entries = store.entries();
    assert!(!entries.is_empty(), "live store has no entries");

    let mut per_category: BTreeMap<&str, usize> = BTreeMap::new();
    let mut malformed = 0usize;
    for entry in &entries {
        *per_category.entry(entry.collection.0.as_str()).or_default() += 1;
        if entry.meta.is_none() {
            malformed += 1;
        }
        for warning in &entry.warnings {
            eprintln!("  warning: {}: {warning}", entry.key.0);
        }
    }

    eprintln!(
        "live store at {}: {} entries in {wall:.2?}",
        root.display(),
        entries.len()
    );
    for (category, count) in &per_category {
        eprintln!("  {category}: {count} entries");
    }
    eprintln!("  malformed (meta: None): {malformed}");
    for warning in store.warnings() {
        eprintln!("  store warning: {warning}");
    }
}

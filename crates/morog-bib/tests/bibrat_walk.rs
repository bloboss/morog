//! Walk tests for [`BibratStore`] over the committed fixture tree
//! (`tests/fixtures/bibrat`), which exercises every observed live-store
//! shape: categories, staging dirs, malformed `.bib`, key mismatch,
//! reference files, per-entry `TAGS.yml`, and `Snapshot/` internals.

use std::path::Path;

use morog_bib::{BibStore, BibratStore, CiteKey};

fn fixture_root() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/bibrat"
    ))
}

fn store() -> BibratStore {
    BibratStore::open(fixture_root()).expect("open fixture store")
}

#[test]
fn walk_finds_exactly_the_entries() {
    let store = store();
    let keys: Vec<&str> = store.entries().iter().map(|e| e.key.0.as_str()).collect();
    // Sorted (BTreeMap order); staging dirs, dotfiles, root files, the
    // nested non-entry dir, Snapshot/ internals, and reference files never
    // become entries.
    assert_eq!(
        keys,
        [
            "broken_2019",
            "jones_2021",
            "smith_2020",
            "snapshot_2023",
            "tagged_2022",
            "unique_2024"
        ]
    );
    assert!(store.resolve(&CiteKey::new("skipped_2020")).is_none());
    assert!(store.resolve(&CiteKey::new("deeper")).is_none());
}

#[test]
fn metadata_title_year_doi() {
    let store = store();
    let smith = store.resolve(&CiteKey::new("smith_2020")).unwrap();
    assert_eq!(smith.collection.0, "alpha");
    let meta = smith.meta.as_ref().expect("smith_2020 parses");
    assert_eq!(
        meta.title.as_deref(),
        Some("Adaptive Plasticity in Deep Networks")
    );
    assert_eq!(meta.year, Some(2020));
    assert_eq!(meta.doi.as_deref(), Some("10.1000/182"));
    assert!(smith.warnings.is_empty());

    let unique = store.resolve(&CiteKey::new("unique_2024")).unwrap();
    assert_eq!(unique.collection.0, "beta");
    assert_eq!(
        unique.meta.as_ref().unwrap().doi.as_deref(),
        Some("10.5555/beta")
    );
}

#[test]
fn malformed_bib_is_a_warning_not_a_failure() {
    let store = store();
    let broken = store.resolve(&CiteKey::new("broken_2019")).unwrap();
    assert!(broken.meta.is_none());
    assert_eq!(broken.warnings.len(), 1);
    assert!(broken.warnings[0].contains("malformed citation.bib"));
}

#[test]
fn directory_name_is_canonical_over_inner_citekey() {
    let store = store();
    // The key is the directory name even though the .bib says jonesWrongKey.
    let jones = store.resolve(&CiteKey::new("jones_2021")).unwrap();
    assert!(store.resolve(&CiteKey::new("jonesWrongKey")).is_none());
    // Metadata still parses, and the mismatch is reported on the entry.
    assert_eq!(jones.meta.as_ref().unwrap().year, Some(2021));
    assert!(
        jones.warnings.iter().any(|w| w.contains("jonesWrongKey")),
        "warnings: {:?}",
        jones.warnings
    );
}

#[test]
fn reference_files_record_extra_memberships() {
    let store = store();
    let smith = store.resolve(&CiteKey::new("smith_2020")).unwrap();
    assert_eq!(
        smith
            .also_in
            .iter()
            .map(|c| c.0.as_str())
            .collect::<Vec<_>>(),
        ["beta"]
    );
    // The dangling reference is a store-level warning.
    assert!(
        store
            .warnings()
            .iter()
            .any(|w| w.contains("dangling_ref") && w.contains("alpha/nope")),
        "warnings: {:?}",
        store.warnings()
    );
}

#[test]
fn nested_non_entry_directory_is_skipped_with_warning() {
    let store = store();
    assert!(
        store
            .warnings()
            .iter()
            .any(|w| w.contains("nested") && w.contains("deferred")),
        "warnings: {:?}",
        store.warnings()
    );
}

#[test]
fn tags_root_vocabulary_and_per_entry() {
    let store = store();
    assert_eq!(store.tags(), ["plasticity", "methods"]);
    let tagged = store.resolve(&CiteKey::new("tagged_2022")).unwrap();
    assert_eq!(tagged.tags, ["plasticity"]);
    // Snapshot/ internals are ignored, not warned about.
    let snap = store.resolve(&CiteKey::new("snapshot_2023")).unwrap();
    assert!(snap.warnings.is_empty());
}

#[test]
fn trait_discover_reloads() {
    let mut store = store();
    store
        .discover(Path::new("/irrelevant-vault"))
        .expect("reload");
    assert_eq!(store.entries().len(), 6);
}

#[test]
fn open_missing_root_errors() {
    let err = BibratStore::open("/nonexistent/bibrat-root").unwrap_err();
    assert!(err.to_string().contains("does not exist"));
}

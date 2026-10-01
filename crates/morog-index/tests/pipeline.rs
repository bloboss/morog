//! Pipeline integration: full and incremental runs over a throwaway vault
//! (plan §3.2), schema-version rebuilds (§3.1), and deletion purges.

use std::path::{Path, PathBuf};

use morog_index::{Error, index_vault, run_query, schema};

/// Temp directory that cleans up after itself.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "morog-index-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn seed_vault(vault: &Path) {
    write(
        vault,
        "topics/sla.md",
        "---\nid: sla\nkind: topic\n---\n\nSLA anchors the spectrum [@wright_leaf_2004]. #anchor evidence-1\n",
    );
    write(
        vault,
        "arguments/a1.morg",
        "---\nid: a1\nkind: argument\nform: claim\nrelations:\n  - { to: sla#evidence-1, stance: supports, op: analyze }\n---\n\nBody.\n",
    );
    write(vault, "plain.md", "No frontmatter here.\n");
    write(vault, "notes.txt", "not a note, wrong extension\n");
    write(vault, ".hidden.md", "dotfiles are skipped\n");
}

fn count(db: &Path, sql: &str) -> i64 {
    match &run_query(db, sql).unwrap().rows[0][0] {
        morog_index::Value::Integer(n) => *n,
        other => panic!("expected integer, got {other:?}"),
    }
}

#[test]
fn full_then_incremental_then_mutations() {
    let tmp = TempDir::new("mutations");
    let vault = tmp.path().join("vault");
    let db = tmp.path().join("cache/index.sqlite");
    seed_vault(&vault);

    // Cold full build: both notes plus the plain file; txt and dotfile skipped.
    let stats = index_vault(&vault, &db, true).unwrap();
    assert_eq!(
        (stats.scanned, stats.changed, stats.indexed, stats.deleted),
        (3, 3, 3, 0)
    );
    assert!(db.exists());
    assert!(!db.with_extension("sqlite.tmp").exists());
    assert_eq!(count(&db, "SELECT COUNT(*) FROM files"), 3);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM notes"), 2);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM plain"), 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM v_edges"), 1);

    // Warm rescan: stats match, nothing is re-parsed.
    let stats = index_vault(&vault, &db, false).unwrap();
    assert_eq!(
        (stats.scanned, stats.changed, stats.indexed, stats.deleted),
        (3, 0, 0, 0)
    );

    // Touch without a content change: stat refresh, no re-parse.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&vault, "plain.md", "No frontmatter here.\n");
    let stats = index_vault(&vault, &db, false).unwrap();
    assert_eq!((stats.changed, stats.indexed), (1, 0));

    // Real edit: one file re-indexed, facts replaced.
    write(
        &vault,
        "topics/sla.md",
        "---\nid: sla\nkind: topic\n---\n\nRewritten body, no cites. #mine\n",
    );
    let stats = index_vault(&vault, &db, false).unwrap();
    assert_eq!((stats.changed, stats.indexed, stats.deleted), (1, 1, 0));
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM relations WHERE kind = 'cites'"),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM blocks_fts WHERE blocks_fts MATCH 'Rewritten'"
        ),
        1
    );

    // New file appears; old file disappears: purge rows, keep FTS in sync.
    write(
        &vault,
        "topics/new.md",
        "---\nid: new-topic\nkind: topic\n---\n\nFresh [@reich_global_1997].\n",
    );
    std::fs::remove_file(vault.join("arguments/a1.morg")).unwrap();
    let stats = index_vault(&vault, &db, false).unwrap();
    assert_eq!(
        (stats.scanned, stats.changed, stats.indexed, stats.deleted),
        (3, 1, 1, 1)
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM files"), 3);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM v_edges"), 0);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM notes WHERE note_id = 'a1'"),
        0
    );
}

#[test]
fn version_mismatch_rebuilds_in_place() {
    let tmp = TempDir::new("version");
    let vault = tmp.path().join("vault");
    let db = tmp.path().join("index.sqlite");
    seed_vault(&vault);

    index_vault(&vault, &db, false).unwrap();

    // Force a bogus schema version; the next run must drop and rebuild.
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);

    assert!(matches!(
        run_query(&db, "SELECT 1").unwrap_err(),
        Error::SchemaVersion { found: 99, .. }
    ));

    let stats = index_vault(&vault, &db, false).unwrap();
    assert_eq!((stats.scanned, stats.indexed), (3, 3));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM notes"), 2);
}

#[test]
fn query_is_read_only() {
    let tmp = TempDir::new("readonly");
    let vault = tmp.path().join("vault");
    let db = tmp.path().join("index.sqlite");
    seed_vault(&vault);
    index_vault(&vault, &db, true).unwrap();

    for sql in [
        "DELETE FROM files",
        "INSERT INTO meta (key, value) VALUES ('x', 'y')",
        "PRAGMA user_version = 3",
        "WITH x AS (SELECT 1) DELETE FROM files",
    ] {
        assert!(
            matches!(run_query(&db, sql), Err(Error::NotSelect)),
            "`{sql}` was not rejected"
        );
    }

    // Statement batches cannot smuggle writes either.
    assert!(run_query(&db, "SELECT 1; DELETE FROM files").is_err());

    let out = run_query(&db, "SELECT note_id FROM notes ORDER BY note_id").unwrap();
    assert_eq!(out.columns, vec!["note_id"]);
    assert_eq!(out.rows.len(), 2);

    assert!(matches!(
        run_query(&tmp.path().join("nope.sqlite"), "SELECT 1"),
        Err(Error::MissingIndex(_))
    ));
}

#[test]
fn full_rebuild_is_atomic_over_existing_db() {
    let tmp = TempDir::new("atomic");
    let vault = tmp.path().join("vault");
    let db = tmp.path().join("index.sqlite");
    seed_vault(&vault);

    index_vault(&vault, &db, false).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM files"), 3);

    // A second full build replaces the database wholesale.
    std::fs::remove_file(vault.join("plain.md")).unwrap();
    let stats = index_vault(&vault, &db, true).unwrap();
    assert_eq!((stats.scanned, stats.indexed), (2, 2));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM files"), 2);
    assert!(schema::open_ro(&db).is_ok());
}

#[test]
fn cold_start_loaders_round_trip_the_whole_index() {
    let tmp = TempDir::new("cold-start");
    let vault = tmp.path().join("vault");
    let db = tmp.path().join("cache/index.sqlite");
    seed_vault(&vault);

    let mut bib = morog_bib::FixtureStore::from_keys(["wright_leaf_2004"]);
    morog_bib::BibStore::discover(&mut bib, &vault).unwrap();
    morog_index::index_vault_with_bib(&vault, &db, true, Some(&bib)).unwrap();

    let conn = schema::open_ro(&db).unwrap();

    // load_all_facts sees every indexed file, and each entry equals the
    // per-file load.
    let all = morog_index::load_all_facts(&conn).unwrap();
    let mut paths: Vec<&str> = all.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["arguments/a1.morg", "plain.md", "topics/sla.md"]);
    for (path, facts) in &all {
        let by_path = morog_index::load_facts_for_path(&conn, path)
            .unwrap()
            .expect("path is indexed");
        assert_eq!(&by_path, facts, "{path}");
    }
    let sla = all
        .iter()
        .find(|(p, _)| p == "topics/sla.md")
        .map(|(_, f)| f)
        .unwrap();
    assert_eq!(sla.note.id.as_ref().unwrap().as_str(), "sla");
    assert_eq!(sla.blocks[0].anchor.as_deref(), Some("evidence-1"));

    assert!(
        morog_index::load_facts_for_path(&conn, "missing.md")
            .unwrap()
            .is_none()
    );

    // The bibliography snapshot comes back out of bib_entries.
    let entries = morog_index::load_bib_entries(&conn).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key.0, "wright_leaf_2004");
}

#[test]
fn tag_table_change_drops_and_rebuilds() {
    use morg_parser::{TagDeclaration, TagTable};

    let tmp = TempDir::new("tag-table");
    let vault = tmp.path().join("vault");
    let db = tmp.path().join("index.sqlite");
    seed_vault(&vault);
    write(
        &vault,
        "reading.md",
        "---\nid: reading\nkind: topic\n---\n\nRead [@k]. #book \"Dune\" by Frank Herbert\n",
    );

    let book_table = TagTable::build([TagDeclaration {
        name: "book".to_owned(),
        pattern: Some(r#""(?<title>[^"]+)"\s+by\s+(?<author>.+)"#.to_owned()),
        kind: None,
    }])
    .unwrap();

    // Build under the empty table: #book stays an unknown tag.
    morog_index::index_vault_with_bib(&vault, &db, false, None).unwrap();
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM blocks WHERE customs IS NOT NULL"),
        0
    );

    // Same (empty) table: a plain warm rescan, nothing re-parsed.
    let stats = morog_index::index_vault_with_bib(&vault, &db, false, None).unwrap();
    assert_eq!((stats.changed, stats.indexed), (0, 0));

    // A different resolved table turns the incremental run into a full
    // drop-and-rebuild: every file is re-parsed under the new table.
    let stats = morog_index::index_vault_with_tags(&vault, &db, false, None, &book_table).unwrap();
    assert_eq!((stats.scanned, stats.indexed), (4, 4));
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM blocks WHERE customs IS NOT NULL"),
        1
    );

    // The occurrences read straight from the index, no vault re-walk.
    let hits = morog_index::tag_occurrences(&db, "book").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "reading.md");
    assert_eq!(hits[0].line, 6);
    assert!(!hits[0].shape_mismatch);
    assert_eq!(
        hits[0].fields,
        [
            ("title".to_owned(), "Dune".to_owned()),
            ("author".to_owned(), "Frank Herbert".to_owned()),
        ]
    );
    assert!(
        morog_index::tag_occurrences(&db, "nope")
            .unwrap()
            .is_empty()
    );

    // Same table again (even though it was built elsewhere): stable, no
    // rebuild — the fingerprint matches.
    let same_table = TagTable::build([TagDeclaration {
        name: "book".to_owned(),
        pattern: Some(r#""(?<title>[^"]+)"\s+by\s+(?<author>.+)"#.to_owned()),
        kind: None,
    }])
    .unwrap();
    let stats = morog_index::index_vault_with_tags(&vault, &db, false, None, &same_table).unwrap();
    assert_eq!((stats.changed, stats.indexed), (0, 0));

    // Dropping the declarations rebuilds once more; the customs disappear.
    let stats = morog_index::index_vault_with_bib(&vault, &db, false, None).unwrap();
    assert_eq!(stats.indexed, 4);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM blocks WHERE customs IS NOT NULL"),
        0
    );

    // A missing index is reported, not built, by the occurrence reader.
    assert!(matches!(
        morog_index::tag_occurrences(&tmp.path().join("nope.sqlite"), "book"),
        Err(Error::MissingIndex(_))
    ));
}

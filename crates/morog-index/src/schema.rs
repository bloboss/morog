//! SQLite schema v1 (plan §3.3) and its lifecycle.
//!
//! The schema follows plan §3.3 exactly, plus the extra columns that §5.4's
//! lossless `FileFacts` round-trip demands (spans, citation locators, raw
//! relation targets, alias order, body word counts). Extra columns are
//! appended after the planned ones and documented inline.
//!
//! Versioning is `PRAGMA user_version`; on any mismatch the database file is
//! deleted and rebuilt (plan §3.1 principle 4 — rebuild, never migrate).

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use crate::Error;

/// Bumped on any schema change; a mismatch triggers a full rebuild.
/// v2 appended `notes.created` and `notes.extra` for the publish exporter.
/// v3 appended `blocks.customs` (user-declared custom-tag occurrences as
/// canonical JSON, plan §10 T1) and introduced the `tag_table_hash` meta
/// key that keys the cache on the resolved tag table.
pub const SCHEMA_VERSION: i64 = 3;

/// Schema v1. Tables and views per plan §3.3; columns past the planned set
/// exist so `FileFacts` round-trips losslessly (plan §5.4):
///
/// - `files.body_words` — `FileFacts::body_words`.
/// - `notes.id_start/id_end/id_line/id_col`, and the same on `plain` —
///   `NoteFacts::id_span` (plain morg notes can carry an `id:` key too).
/// - `aliases.file_id`, `aliases.ord` — per-file purge and alias order.
/// - `blocks.col`, `blocks.citable` — the block span's column and the
///   provenance-rule applicability flag.
/// - `relations.start_byte/end_byte` — the span bytes (`line`/`col` alone
///   are lossy); `relations.locator` — `[@key, p. 12]` locators on `cites`
///   rows; `relations.to_raw` — the raw frontmatter target text, which can
///   differ from `dst || '#' || dst_anchor` (e.g. a trailing `#`).
/// - `notes.created`, `notes.extra` (v2) — the `created:` value and the
///   user-owned `extra:` map as JSON, so `morog publish` exposes frontmatter
///   to templates straight from the index (plan §6.2–6.3).
/// - `blocks.customs` (v3) — the block's user-declared custom-tag
///   occurrences (name, span-carrying fields, shape mismatch) as canonical
///   JSON, mirroring the `notes.extra` approach; NULL when the block
///   carries none. Feeds `morog tags` and the LSP without a reparse.
const SCHEMA_SQL: &str = "
CREATE TABLE meta      (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE files     (file_id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL,
                        mtime_ns INTEGER, size INTEGER, hash BLOB,
                        body_words INTEGER NOT NULL DEFAULT 0);
CREATE TABLE notes     (note_id TEXT PRIMARY KEY, uuid TEXT UNIQUE,
                        kind TEXT NOT NULL CHECK (kind IN ('source','topic','argument')),
                        title TEXT, form TEXT, thesis TEXT, status TEXT,
                        publish INTEGER NOT NULL DEFAULT 0,
                        file_id INTEGER NOT NULL REFERENCES files ON DELETE CASCADE,
                        id_start INTEGER, id_end INTEGER, id_line INTEGER, id_col INTEGER,
                        created TEXT, extra TEXT);
CREATE TABLE plain     (file_id INTEGER PRIMARY KEY REFERENCES files ON DELETE CASCADE,
                        id_start INTEGER, id_end INTEGER, id_line INTEGER, id_col INTEGER);
CREATE TABLE aliases   (note_id TEXT REFERENCES notes ON DELETE CASCADE, alias TEXT,
                        file_id INTEGER REFERENCES files ON DELETE CASCADE,
                        ord INTEGER);
CREATE TABLE blocks    (file_id INTEGER REFERENCES files ON DELETE CASCADE, idx INTEGER,
                        anchor TEXT, unit TEXT CHECK (unit IN ('evidence','mine','none')),
                        start_byte INTEGER, end_byte INTEGER, line INTEGER, text TEXT,
                        col INTEGER, citable INTEGER NOT NULL DEFAULT 0,
                        customs TEXT,
                        PRIMARY KEY (file_id, idx));
CREATE TABLE relations (file_id INTEGER REFERENCES files ON DELETE CASCADE,
                        src_note TEXT, src_block INTEGER,
                        dst TEXT NOT NULL, dst_anchor TEXT,
                        kind TEXT NOT NULL, stance TEXT, op INTEGER,
                        origin TEXT CHECK (origin IN ('frontmatter','inline')),
                        line INTEGER, col INTEGER,
                        start_byte INTEGER, end_byte INTEGER,
                        locator TEXT, to_raw TEXT);
CREATE TABLE bib_entries (citekey TEXT PRIMARY KEY, collection TEXT, dir TEXT,
                          title TEXT, year INTEGER, doi TEXT);

CREATE INDEX idx_notes_file     ON notes (file_id);
CREATE INDEX idx_aliases_file   ON aliases (file_id);
CREATE INDEX idx_relations_file ON relations (file_id);
CREATE INDEX idx_relations_dst  ON relations (dst);

CREATE VIRTUAL TABLE blocks_fts USING fts5(text, content='blocks', content_rowid='rowid');

-- Resolved relations: rows whose dst is a known note id.
CREATE VIEW v_edges AS
SELECT r.src_note AS src, r.dst AS dst, r.kind AS kind,
       r.stance AS stance, r.op AS op, n.kind AS dst_kind
FROM relations r
JOIN notes n ON n.note_id = r.dst;

-- v_edges inverted: incoming edges, grouped (ordered) by target.
CREATE VIEW v_backlinks AS
SELECT dst AS target, dst_kind AS target_kind, src, kind, stance, op
FROM v_edges
ORDER BY dst, src;

-- Relations whose dst does not resolve: to a note id, to that note's
-- anchors, or (for cites) to a bibrat citekey.
CREATE VIEW v_dangling AS
SELECT f.path AS path, r.src_note AS src, r.src_block AS src_block,
       r.kind AS kind, r.dst AS dst, r.dst_anchor AS dst_anchor,
       r.line AS line, r.col AS col,
       CASE
         WHEN r.kind = 'cites' THEN 'unknown-citekey'
         WHEN n.note_id IS NULL THEN 'unknown-note'
         ELSE 'unknown-anchor'
       END AS reason
FROM relations r
JOIN files f ON f.file_id = r.file_id
LEFT JOIN notes n ON n.note_id = r.dst
WHERE (r.kind = 'cites'
       AND NOT EXISTS (SELECT 1 FROM bib_entries b WHERE b.citekey = r.dst))
   OR (r.kind <> 'cites'
       AND (n.note_id IS NULL
            OR (r.dst_anchor IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM blocks b
                                WHERE b.file_id = n.file_id
                                  AND b.anchor = r.dst_anchor))));

-- Topic blocks with unit='evidence', one row per carried citation.
CREATE VIEW v_evidence AS
SELECT n.note_id AS topic, b.idx AS idx, b.anchor AS anchor,
       r.dst AS citekey, r.locator AS locator, b.text AS text
FROM notes n
JOIN blocks b ON b.file_id = n.file_id
JOIN relations r ON r.file_id = b.file_id
                AND r.src_block = b.idx
                AND r.kind = 'cites'
WHERE n.kind = 'topic' AND b.unit = 'evidence';

-- Bloom depth per argument: MAX(op) over its typed relations.
CREATE VIEW v_argument_depth AS
SELECT n.note_id AS argument, MAX(r.op) AS depth
FROM notes n
LEFT JOIN relations r ON r.src_note = n.note_id AND r.kind = 'argues'
WHERE n.kind = 'argument'
GROUP BY n.note_id;

-- Topics no argument relates to; sources never cited.
CREATE VIEW v_orphans AS
SELECT n.note_id AS note, n.kind AS kind
FROM notes n
WHERE (n.kind = 'topic'
       AND NOT EXISTS (SELECT 1 FROM relations r
                       WHERE r.kind = 'argues' AND r.dst = n.note_id))
   OR (n.kind = 'source'
       AND NOT EXISTS (SELECT 1 FROM relations r
                       WHERE r.kind = 'cites' AND r.dst = n.note_id));
";

/// Triggers that keep the external-content `blocks_fts` in sync with
/// `blocks` during incremental updates. Full rebuilds bulk-load without
/// them and run one FTS `rebuild` instead ([`create_schema_bulk`] /
/// [`finish_bulk`]), which is several times faster than per-row inserts.
const TRIGGERS_SQL: &str = "
CREATE TRIGGER blocks_ai AFTER INSERT ON blocks BEGIN
  INSERT INTO blocks_fts (rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER blocks_ad AFTER DELETE ON blocks BEGIN
  INSERT INTO blocks_fts (blocks_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TRIGGER blocks_au AFTER UPDATE ON blocks BEGIN
  INSERT INTO blocks_fts (blocks_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
  INSERT INTO blocks_fts (rowid, text) VALUES (new.rowid, new.text);
END;
";

/// Create the v1 schema in an empty database and stamp `user_version`.
pub fn create_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(SCHEMA_SQL)?;
    conn.execute_batch(TRIGGERS_SQL)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// Create the schema for a bulk (full) build: no FTS sync triggers yet, and
/// no journal or fsyncs — the build targets a temp file that is atomically
/// renamed in only after success, so a crash loses nothing.
pub fn create_schema_bulk(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "journal_mode", "OFF")?;
    conn.pragma_update(None, "synchronous", "OFF")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    conn.execute_batch(SCHEMA_SQL)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// Finish a bulk build: populate `blocks_fts` in one rebuild pass and attach
/// the sync triggers for subsequent incremental runs.
pub fn finish_bulk(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch("INSERT INTO blocks_fts (blocks_fts) VALUES ('rebuild');")?;
    conn.execute_batch(TRIGGERS_SQL)?;
    Ok(())
}

/// Apply the connection pragmas every writer uses (plan §3.1 principle 3).
pub fn writer_pragmas(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(())
}

fn user_version(conn: &Connection) -> Result<i64, Error> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Remove a database file together with its WAL sidecar files.
pub fn remove_db(db_path: &Path) -> std::io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut os = db_path.as_os_str().to_owned();
        os.push(suffix);
        match std::fs::remove_file(Path::new(&os)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Open (or create) the index database for writing, enforcing the schema
/// version. On a `user_version` mismatch the file is deleted and rebuilt
/// from scratch (plan §3.1 principle 4).
///
/// Returns the connection and whether a fresh schema was created.
pub fn open_rw(db_path: &Path) -> Result<(Connection, bool), Error> {
    if let Some(dir) = db_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(db_path)?;
    writer_pragmas(&conn)?;
    let version = user_version(&conn)?;
    if version == SCHEMA_VERSION {
        return Ok((conn, false));
    }
    if version == 0 && is_empty(&conn)? {
        create_schema(&conn)?;
        return Ok((conn, true));
    }
    // Version mismatch (or an unversioned non-empty file): drop and rebuild.
    drop(conn);
    remove_db(db_path)?;
    let conn = Connection::open(db_path)?;
    writer_pragmas(&conn)?;
    create_schema(&conn)?;
    Ok((conn, true))
}

/// Open the index database read-only, with `query_only` as a second fence.
pub fn open_ro(db_path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.pragma_update(None, "query_only", true)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let version = user_version(&conn)?;
    if version != SCHEMA_VERSION {
        return Err(Error::SchemaVersion {
            found: version,
            expected: SCHEMA_VERSION,
        });
    }
    Ok(conn)
}

fn is_empty(conn: &Connection) -> Result<bool, Error> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))?;
    Ok(n == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_and_stamps_version() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
        for object in [
            "meta",
            "files",
            "notes",
            "plain",
            "aliases",
            "blocks",
            "relations",
            "bib_entries",
            "blocks_fts",
            "v_edges",
            "v_backlinks",
            "v_dangling",
            "v_evidence",
            "v_argument_depth",
            "v_orphans",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                    [object],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(n >= 1, "missing schema object {object}");
        }
    }

    #[test]
    fn fts_triggers_track_blocks() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (path, mtime_ns, size) VALUES ('a.md', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO blocks (file_id, idx, unit, text) VALUES (1, 0, 'none', 'leaf economics')",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blocks_fts WHERE blocks_fts MATCH 'economics'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
        conn.execute("DELETE FROM blocks WHERE file_id = 1", [])
            .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM blocks_fts WHERE blocks_fts MATCH 'economics'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 0);
    }
}

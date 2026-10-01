//! Storing and loading `FileFacts` as rows.
//!
//! `load_file_facts(store_file_facts(facts)) == facts` is the contract
//! (plan §5.4) — the LSP cold-starts from a table scan instead of a reparse,
//! so every field of [`FileFacts`] must survive the trip. The property test
//! in `tests/roundtrip.rs` enforces this over generated note shapes.

use morg_parser::Span;
use morog_sema::{
    BlockFacts, BlockUnit, BloomOp, CiteRef, FileFacts, IdRef, NoteFacts, NoteId, RelationFacts,
    RelationKind, Stance,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::Error;

/// Relation `kind` strings as stored in the `relations` table.
mod kind {
    pub const ARGUES: &str = "argues";
    pub const RELATED: &str = "related";
    pub const CITES: &str = "cites";
    pub const MENTIONS: &str = "mentions";
}

/// Store one file's facts under an existing `files.file_id` row.
///
/// Expects a clean slate for `file_id` (fresh row, or [`delete_file_rows`]
/// first). `source` supplies the block text stored for FTS. Runs inside the
/// caller's transaction. A note whose `note_id` or `uuid` collides with an
/// already-stored note keeps its file/blocks/relations rows but drops the
/// `notes` row (`INSERT OR IGNORE`); the duplicate itself is a lint
/// (`sta::id`), not an indexing failure.
pub fn store_file_facts(
    conn: &Connection,
    file_id: i64,
    source: &str,
    facts: &FileFacts,
) -> Result<(), Error> {
    let note = &facts.note;
    let id_span = note.id_span;
    let note_id = note.id.as_ref().map(|id| id.as_str().to_owned());

    conn.prepare_cached("UPDATE files SET body_words = ?1 WHERE file_id = ?2")?
        .execute(params![facts.body_words, file_id])?;

    match note.kind {
        None => {
            conn.prepare_cached(
                "INSERT INTO plain (file_id, id_start, id_end, id_line, id_col)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                file_id,
                id_span.map(|s| s.start as i64),
                id_span.map(|s| s.end as i64),
                id_span.map(|s| s.line),
                id_span.map(|s| s.col),
            ])?;
        }
        Some(kind) => {
            conn.prepare_cached(
                "INSERT OR IGNORE INTO notes
                   (note_id, uuid, kind, title, form, thesis, status, publish, file_id,
                    id_start, id_end, id_line, id_col, created, extra)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            )?
            .execute(params![
                note_id,
                note.uuid.map(|u| u.to_string()),
                kind.as_str(),
                note.title,
                note.form.map(|f| f.as_str()),
                note.thesis,
                note.status,
                note.publish,
                file_id,
                id_span.map(|s| s.start as i64),
                id_span.map(|s| s.end as i64),
                id_span.map(|s| s.line),
                id_span.map(|s| s.col),
                note.created,
                note.extra_json,
            ])?;
            let mut insert_alias = conn.prepare_cached(
                "INSERT INTO aliases (note_id, alias, file_id, ord) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (ord, alias) in note.aliases.iter().enumerate() {
                insert_alias.execute(params![note_id, alias, file_id, ord as i64])?;
            }
        }
    }

    let mut insert_block = conn.prepare_cached(
        "INSERT INTO blocks
           (file_id, idx, anchor, unit, start_byte, end_byte, line, text, col, citable)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut insert_relation = conn.prepare_cached(
        "INSERT INTO relations
           (file_id, src_note, src_block, dst, dst_anchor, kind, stance, op, origin,
            line, col, start_byte, end_byte, locator, to_raw)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;

    for block in &facts.blocks {
        let span = block.span;
        let text = source.get(span.start..span.end).unwrap_or("");
        insert_block.execute(params![
            file_id,
            block.idx,
            block.anchor,
            block.unit.as_str(),
            span.start as i64,
            span.end as i64,
            span.line,
            text,
            span.col,
            block.citable,
        ])?;
        for cite in &block.cites {
            insert_relation.execute(params![
                file_id,
                note_id,
                block.idx,
                cite.key.0,
                Option::<String>::None,
                kind::CITES,
                Option::<String>::None,
                Option::<i64>::None,
                "inline",
                cite.span.line,
                cite.span.col,
                cite.span.start as i64,
                cite.span.end as i64,
                cite.locator,
                Option::<String>::None,
            ])?;
        }
    }

    for rel in &facts.relations {
        let (kind_str, stance, op) = match rel.kind {
            RelationKind::Argues { stance, op } => (kind::ARGUES, Some(stance), op),
            RelationKind::Related => (kind::RELATED, None, None),
            // Frontmatter relations are only argues/related (plan §2.3).
            RelationKind::Cites | RelationKind::Mentions => unreachable!("inline-only kind"),
        };
        insert_relation.execute(params![
            file_id,
            note_id,
            Option::<i64>::None,
            rel.target_id,
            rel.target_anchor,
            kind_str,
            stance.map(|s| s.as_str()),
            op.map(|o| o.ordinal() as i64),
            "frontmatter",
            rel.span.line,
            rel.span.col,
            rel.span.start as i64,
            rel.span.end as i64,
            Option::<String>::None,
            rel.to,
        ])?;
    }

    for id_ref in &facts.id_refs {
        insert_relation.execute(params![
            file_id,
            note_id,
            id_ref.block_idx,
            id_ref.target_id,
            id_ref.target_anchor,
            kind::MENTIONS,
            Option::<String>::None,
            Option::<i64>::None,
            "inline",
            id_ref.span.line,
            id_ref.span.col,
            id_ref.span.start as i64,
            id_ref.span.end as i64,
            Option::<String>::None,
            Option::<String>::None,
        ])?;
    }

    Ok(())
}

/// Load one file's facts back from its rows — the inverse of
/// [`store_file_facts`].
pub fn load_file_facts(conn: &Connection, file_id: i64) -> Result<FileFacts, Error> {
    let body_words: u32 = conn
        .prepare_cached("SELECT body_words FROM files WHERE file_id = ?1")?
        .query_row([file_id], |r| r.get(0))?;

    let note = load_note_facts(conn, file_id)?;
    let mut blocks = load_blocks(conn, file_id)?;

    // Inline citations, in insertion (document) order, attached per block.
    let mut cites_stmt = conn.prepare_cached(
        "SELECT src_block, dst, locator, start_byte, end_byte, line, col
         FROM relations WHERE file_id = ?1 AND kind = 'cites' ORDER BY rowid",
    )?;
    let cite_rows = cites_stmt.query_map([file_id], |r| {
        Ok((
            r.get::<_, u32>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            row_span(r, 3)?,
        ))
    })?;
    for row in cite_rows {
        let (block_idx, key, locator, span) = row?;
        let block = blocks
            .get_mut(block_idx as usize)
            .filter(|b| b.idx == block_idx)
            .ok_or_else(|| Error::Corrupt(format!("cite row for missing block {block_idx}")))?;
        block.cites.push(CiteRef {
            key: morog_sema::CiteKey::new(key),
            locator,
            span,
        });
    }

    let mut rel_stmt = conn.prepare_cached(
        "SELECT kind, dst, dst_anchor, stance, op, to_raw, start_byte, end_byte, line, col
         FROM relations WHERE file_id = ?1 AND origin = 'frontmatter' ORDER BY rowid",
    )?;
    let relations = rel_stmt
        .query_map([file_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
                row_span(r, 6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let relations = relations
        .into_iter()
        .map(|(kind_str, dst, dst_anchor, stance, op, to_raw, span)| {
            let kind = match kind_str.as_str() {
                kind::ARGUES => RelationKind::Argues {
                    stance: stance
                        .as_deref()
                        .and_then(Stance::parse)
                        .ok_or_else(|| Error::Corrupt(format!("bad stance {stance:?}")))?,
                    op: op.map(bloom_from_ordinal).transpose()?,
                },
                kind::RELATED => RelationKind::Related,
                other => return Err(Error::Corrupt(format!("bad frontmatter kind `{other}`"))),
            };
            Ok(RelationFacts {
                kind,
                to: to_raw.unwrap_or_else(|| join_target(&dst, dst_anchor.as_deref())),
                target_id: dst,
                target_anchor: dst_anchor,
                span,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;

    let mut refs_stmt = conn.prepare_cached(
        "SELECT dst, dst_anchor, src_block, start_byte, end_byte, line, col
         FROM relations WHERE file_id = ?1 AND kind = 'mentions' ORDER BY rowid",
    )?;
    let id_refs = refs_stmt
        .query_map([file_id], |r| {
            Ok(IdRef {
                target_id: r.get(0)?,
                target_anchor: r.get(1)?,
                block_idx: r.get(2)?,
                span: row_span(r, 3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(FileFacts {
        note,
        blocks,
        relations,
        id_refs,
        body_words,
    })
}

/// Load every file's facts, keyed by vault-relative path — the LSP's
/// cold start (plan §5.4): one table scan, no reparse.
pub fn load_all_facts(conn: &Connection) -> Result<Vec<(String, FileFacts)>, Error> {
    let files: Vec<(i64, String)> = conn
        .prepare_cached("SELECT file_id, path FROM files ORDER BY file_id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    files
        .into_iter()
        .map(|(file_id, path)| Ok((path, load_file_facts(conn, file_id)?)))
        .collect()
}

/// Load one file's facts by its vault-relative path; `None` when the index
/// has no row for it (e.g. just deleted).
pub fn load_facts_for_path(conn: &Connection, rel_path: &str) -> Result<Option<FileFacts>, Error> {
    let file_id: Option<i64> = conn
        .prepare_cached("SELECT file_id FROM files WHERE path = ?1")?
        .query_row([rel_path], |r| r.get(0))
        .optional()?;
    file_id.map(|id| load_file_facts(conn, id)).transpose()
}

/// Load the bibliography snapshot back out of `bib_entries` — the inverse
/// of [`store_bib_entries`] up to the fields the table keeps (plan §4.2:
/// key completion and resolution never touch the filesystem).
pub fn load_bib_entries(conn: &Connection) -> Result<Vec<morog_bib::BibEntry>, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT citekey, collection, dir, title, year, doi FROM bib_entries ORDER BY citekey",
    )?;
    let entries = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i32>>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(entries
        .into_iter()
        .map(|(citekey, collection, dir, title, year, doi)| {
            let meta = (title.is_some() || year.is_some() || doi.is_some())
                .then_some(morog_bib::BibMeta { title, year, doi });
            morog_bib::BibEntry {
                key: morog_bib::CiteKey::new(citekey),
                collection: morog_bib::CollectionPath(collection),
                also_in: Vec::new(),
                dir: std::path::PathBuf::from(dir),
                meta,
                tags: Vec::new(),
                warnings: Vec::new(),
            }
        })
        .collect())
}

/// Delete every row belonging to `file_id`, including the `files` row.
///
/// Explicit and ordered rather than FK-cascaded, so the `blocks` delete
/// trigger keeps `blocks_fts` in sync.
pub fn delete_file_rows(conn: &Connection, file_id: i64) -> Result<(), Error> {
    for sql in [
        "DELETE FROM blocks WHERE file_id = ?1",
        "DELETE FROM relations WHERE file_id = ?1",
        "DELETE FROM aliases WHERE file_id = ?1",
        "DELETE FROM notes WHERE file_id = ?1",
        "DELETE FROM plain WHERE file_id = ?1",
        "DELETE FROM files WHERE file_id = ?1",
    ] {
        conn.prepare_cached(sql)?.execute([file_id])?;
    }
    Ok(())
}

/// Replace the `bib_entries` table with the given bibliography snapshot.
///
/// The table is always rewritten in full — the bibliography is small
/// (plan §4) and the index is a disposable cache, so the row set is a pure
/// function of the store that was (or was not) supplied. Runs inside the
/// caller's transaction.
pub fn store_bib_entries(conn: &Connection, entries: &[&morog_bib::BibEntry]) -> Result<(), Error> {
    conn.execute("DELETE FROM bib_entries", [])?;
    let mut stmt = conn.prepare_cached(
        "INSERT OR REPLACE INTO bib_entries (citekey, collection, dir, title, year, doi)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for entry in entries {
        let meta = entry.meta.as_ref();
        stmt.execute(params![
            entry.key.0,
            entry.collection.0,
            entry.dir.display().to_string(),
            meta.and_then(|m| m.title.as_deref()),
            meta.and_then(|m| m.year),
            meta.and_then(|m| m.doi.as_deref()),
        ])?;
    }
    Ok(())
}

fn load_note_facts(conn: &Connection, file_id: i64) -> Result<NoteFacts, Error> {
    type NoteRow = (
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        Option<Span>,
        Option<String>,
        Option<String>,
    );
    let note_row: Option<NoteRow> = conn
        .prepare_cached(
            "SELECT note_id, uuid, kind, title, form, thesis, status, publish,
                    id_start, id_end, id_line, id_col, created, extra
             FROM notes WHERE file_id = ?1",
        )?
        .query_row([file_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                opt_span(r, 8)?,
                r.get(12)?,
                r.get(13)?,
            ))
        })
        .optional()?;

    let Some((note_id, uuid, kind, title, form, thesis, status, publish, id_span, created, extra)) =
        note_row
    else {
        // Plain morg note (or a kinded note whose id collided — see
        // `store_file_facts`; that case does not round-trip and is linted).
        let id_span = conn
            .prepare_cached(
                "SELECT id_start, id_end, id_line, id_col FROM plain WHERE file_id = ?1",
            )?
            .query_row([file_id], |r| opt_span(r, 0))
            .optional()?
            .flatten();
        return Ok(NoteFacts {
            id_span,
            ..NoteFacts::default()
        });
    };

    let aliases = conn
        .prepare_cached("SELECT alias FROM aliases WHERE file_id = ?1 ORDER BY ord")?
        .query_map([file_id], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(NoteFacts {
        kind: morog_sema::NoteKind::parse(&kind),
        id: note_id.map(NoteId::new),
        id_span,
        uuid: uuid.as_deref().and_then(|u| u.parse().ok()),
        title,
        form: form.as_deref().and_then(morog_sema::ArgumentForm::parse),
        thesis,
        status,
        publish,
        created,
        extra_json: extra,
        aliases,
    })
}

fn load_blocks(conn: &Connection, file_id: i64) -> Result<Vec<BlockFacts>, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT idx, anchor, unit, start_byte, end_byte, line, col, citable
         FROM blocks WHERE file_id = ?1 ORDER BY idx",
    )?;
    let blocks = stmt
        .query_map([file_id], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                row_span(r, 3)?,
                r.get::<_, bool>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    blocks
        .into_iter()
        .map(|(idx, anchor, unit, span, citable)| {
            let unit = match unit.as_str() {
                "evidence" => BlockUnit::Evidence,
                "mine" => BlockUnit::Mine,
                "none" => BlockUnit::None,
                other => return Err(Error::Corrupt(format!("bad block unit `{other}`"))),
            };
            Ok(BlockFacts {
                idx,
                anchor,
                unit,
                citable,
                cites: Vec::new(),
                customs: Vec::new(),
                span,
            })
        })
        .collect()
}

/// Read a `(start, end, line, col)` column run starting at `base` as a span.
fn row_span(r: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<Span> {
    Ok(Span::new(
        r.get::<_, i64>(base)? as usize,
        r.get::<_, i64>(base + 1)? as usize,
        r.get(base + 2)?,
        r.get(base + 3)?,
    ))
}

/// Like [`row_span`], but `None` when the columns are NULL.
fn opt_span(r: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<Option<Span>> {
    let start: Option<i64> = r.get(base)?;
    let Some(start) = start else { return Ok(None) };
    Ok(Some(Span::new(
        start as usize,
        r.get::<_, i64>(base + 1)? as usize,
        r.get(base + 2)?,
        r.get(base + 3)?,
    )))
}

fn join_target(dst: &str, anchor: Option<&str>) -> String {
    match anchor {
        Some(a) => format!("{dst}#{a}"),
        None => dst.to_owned(),
    }
}

fn bloom_from_ordinal(ord: i64) -> Result<BloomOp, Error> {
    match ord {
        0 => Ok(BloomOp::Recall),
        1 => Ok(BloomOp::Explain),
        2 => Ok(BloomOp::Apply),
        3 => Ok(BloomOp::Analyze),
        4 => Ok(BloomOp::Evaluate),
        5 => Ok(BloomOp::Create),
        other => Err(Error::Corrupt(format!("bad bloom ordinal {other}"))),
    }
}

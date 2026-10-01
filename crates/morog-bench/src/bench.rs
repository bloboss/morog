//! Timing harness over a generated vault.
//!
//! Three phases, all wall-clock (`std::time::Instant`), single-threaded so
//! the numbers are a conservative floor (plan 3.2 runs parse/extract on a
//! rayon pool):
//!
//! 1. parse  — walk + read + `morg_parser::parse_document` per file
//! 2. index  — walk + read + blake3 + parse + trivial fact extraction +
//!    batched rusqlite inserts (WAL, 256 files/transaction)
//! 3. cold   — SELECT everything back into memory from a fresh connection
//!
//! The results are compared against the plan 5.7 budgets.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use morg_parser::{Block, Document, InlineContent, InlineKind};
use rusqlite::Connection;

pub struct PhaseStats {
    pub total: Duration,
    pub p50: Duration,
    pub p99: Duration,
    pub max: Duration,
}

fn phase_stats(mut per_file: Vec<Duration>, total: Duration) -> PhaseStats {
    per_file.sort_unstable();
    let pick = |pct: usize| -> Duration {
        if per_file.is_empty() {
            Duration::ZERO
        } else {
            per_file[(per_file.len() - 1) * pct / 100]
        }
    };
    PhaseStats {
        total,
        p50: pick(50),
        p99: pick(99),
        max: per_file.last().copied().unwrap_or(Duration::ZERO),
    }
}

pub fn run(vault: &Path, db_path: &Path, batch: usize) -> io::Result<()> {
    // ---- walk -------------------------------------------------------------
    let t_walk = Instant::now();
    let files = walk(vault)?;
    let walk_time = t_walk.elapsed();
    if files.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no .md files under {} — run `gen` first", vault.display()),
        ));
    }

    // ---- phase 1: read + parse ---------------------------------------------
    let mut per_file = Vec::with_capacity(files.len());
    let mut bytes = 0u64;
    let mut parse_errors = 0usize;
    let t_parse = Instant::now();
    for path in &files {
        let t0 = Instant::now();
        let src = std::fs::read_to_string(path)?;
        let result = morg_parser::parse_document(&src);
        per_file.push(t0.elapsed());
        bytes += src.len() as u64;
        parse_errors += result.errors.len();
    }
    let parse = phase_stats(per_file, t_parse.elapsed());

    // ---- phase 2: index pipeline -------------------------------------------
    let _ = std::fs::remove_file(db_path);
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(db_path.with_extension("sqlite-shm"));
    let conn = Connection::open(db_path).map_err(sql_err)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(sql_err)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(sql_err)?;
    conn.execute_batch(
        "CREATE TABLE files (file_id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL,
                             size INTEGER NOT NULL, hash BLOB NOT NULL);
         CREATE TABLE notes (note_id TEXT, kind TEXT, links INTEGER, tags INTEGER,
                             cites INTEGER, file_id INTEGER NOT NULL REFERENCES files);",
    )
    .map_err(sql_err)?;

    let mut per_file = Vec::with_capacity(files.len());
    let t_index = Instant::now();
    for chunk in files.chunks(batch) {
        conn.execute_batch("BEGIN").map_err(sql_err)?;
        {
            let mut ins_file = conn
                .prepare_cached("INSERT INTO files (path, size, hash) VALUES (?1, ?2, ?3)")
                .map_err(sql_err)?;
            let mut ins_note = conn
                .prepare_cached(
                    "INSERT INTO notes (note_id, kind, links, tags, cites, file_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(sql_err)?;
            for path in chunk {
                let t0 = Instant::now();
                let raw = std::fs::read(path)?;
                let hash = blake3::hash(&raw);
                let src = String::from_utf8_lossy(&raw);
                let result = morg_parser::parse_document(&src);
                let facts = extract_facts(&result.document);
                ins_file
                    .execute(rusqlite::params![
                        path.to_string_lossy(),
                        raw.len() as i64,
                        hash.as_bytes().as_slice(),
                    ])
                    .map_err(sql_err)?;
                let file_id = conn.last_insert_rowid();
                ins_note
                    .execute(rusqlite::params![
                        facts.note_id,
                        facts.kind,
                        facts.links as i64,
                        facts.tags as i64,
                        facts.cites as i64,
                        file_id,
                    ])
                    .map_err(sql_err)?;
                per_file.push(t0.elapsed());
            }
        }
        conn.execute_batch("COMMIT").map_err(sql_err)?;
    }
    let index = phase_stats(per_file, t_index.elapsed());
    drop(conn);

    // ---- phase 3: cold load -------------------------------------------------
    let t_cold = Instant::now();
    let conn = Connection::open(db_path).map_err(sql_err)?;
    type NoteRow = (Option<String>, Option<String>, i64, i64, i64, i64);
    let mut notes: Vec<NoteRow> = Vec::new();
    let mut stmt = conn
        .prepare("SELECT note_id, kind, links, tags, cites, file_id FROM notes")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(sql_err)?;
    for row in rows {
        notes.push(row.map_err(sql_err)?);
    }
    let mut file_rows: Vec<(i64, String, i64, Vec<u8>)> = Vec::new();
    let mut stmt = conn
        .prepare("SELECT file_id, path, size, hash FROM files")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(sql_err)?;
    for row in rows {
        file_rows.push(row.map_err(sql_err)?);
    }
    let cold = t_cold.elapsed();

    // In-memory full scan over the loaded rows (proxy for a rename/backlink
    // sweep that never touches disk).
    let needle = notes[notes.len() / 2].0.clone();
    let t_scan = Instant::now();
    let hits = notes.iter().filter(|n| n.0 == needle).count();
    let scan = t_scan.elapsed();

    // ---- report -------------------------------------------------------------
    let n = files.len();
    let mib = bytes as f64 / (1024.0 * 1024.0);
    println!();
    println!("vault: {n} files, {mib:.1} MiB, parse errors: {parse_errors}");
    println!("walk:  {walk_time:>10.2?}  (single-threaded throughout)");
    println!();
    println!(
        "{:<28} {:>10} {:>10} {:>10} {:>10} {:>12}",
        "phase", "total", "p50/file", "p99/file", "max/file", "throughput"
    );
    let row = |name: &str, s: &PhaseStats| {
        println!(
            "{:<28} {:>10.2?} {:>10.2?} {:>10.2?} {:>10.2?} {:>9.1} MiB/s",
            name,
            s.total,
            s.p50,
            s.p99,
            s.max,
            mib / s.total.as_secs_f64()
        );
    };
    row("read + parse", &parse);
    row("hash + parse + extract + db", &index);
    println!(
        "{:<28} {cold:>10.2?}  ({} note rows, {} file rows)",
        "cold load (SELECT all)",
        notes.len(),
        file_rows.len()
    );
    println!("{:<28} {scan:>10.2?}  ({hits} hit)", "in-memory id scan");

    println!();
    println!("plan 5.7 budget check");
    budget(
        "keystroke -> diagnostics < 30 ms (single-file reparse, p99)",
        parse.p99 + index.p99, // reparse + re-extract of one file, pessimistic
        Duration::from_millis(30),
    );
    budget(
        "cold start < 1 s (cold load of warm index)",
        cold,
        Duration::from_secs(1),
    );
    println!(
        "  note: completion (<20 ms) and rename (<200 ms) need the real LSP; the\n  in-memory scan above ({scan:.2?} over {} rows) bounds the per-query table sweep.",
        notes.len()
    );
    Ok(())
}

fn budget(name: &str, measured: Duration, budget: Duration) {
    let verdict = if measured <= budget / 2 {
        "PASS"
    } else if measured <= budget {
        "PASS (tight)"
    } else {
        "OVER"
    };
    println!("  {name}: measured {measured:.2?} -> {verdict}");
}

struct Facts {
    note_id: Option<String>,
    kind: Option<String>,
    links: usize,
    tags: usize,
    cites: usize,
}

#[derive(Default)]
struct Counts {
    links: usize,
    tags: usize,
    cites: usize,
}

/// Trivial fact extraction: frontmatter id/kind plus link, tag, and cite
/// counts from a full AST walk.
fn extract_facts(doc: &Document) -> Facts {
    let fm_str = |key: &str| -> Option<String> {
        doc.frontmatter
            .as_ref()
            .and_then(|fm| fm.data.as_mapping_get(key))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let mut counts = Counts::default();
    count_blocks(&doc.children, &mut counts);
    Facts {
        note_id: fm_str("id"),
        kind: fm_str("kind"),
        links: counts.links,
        tags: counts.tags,
        cites: counts.cites,
    }
}

fn count_blocks(blocks: &[Block], counts: &mut Counts) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => count_inline(&p.content, counts),
            Block::Heading(h) => count_inline(&h.content, counts),
            Block::List(l) => {
                for item in &l.items {
                    count_inline(&item.content, counts);
                    if let Some(desc) = &item.description {
                        count_inline(desc, counts);
                    }
                    count_blocks(&item.children, counts);
                }
            }
            Block::Callout(c) => {
                counts.tags += c.tags.len();
                count_blocks(&c.content, counts);
            }
            Block::Table(t) => {
                for cell in t.headers.iter().chain(t.rows.iter().flatten()) {
                    count_inline(cell, counts);
                }
            }
            Block::FootnoteDefinition(f) => count_inline(&f.content, counts),
            Block::CodeBlock(c) => counts.tags += c.tags.len(),
            Block::BlockTag(_) => counts.tags += 1,
            Block::BlankLine(_)
            | Block::HtmlBlock(_)
            | Block::HorizontalRule(_)
            | Block::Comment(_) => {}
        }
    }
}

fn count_inline(content: &InlineContent, counts: &mut Counts) {
    for seg in &content.segments {
        match &seg.kind {
            InlineKind::Link(link) => {
                counts.links += 1;
                counts.tags += link.tags.len();
            }
            InlineKind::Tag(_) => counts.tags += 1,
            InlineKind::Cite { .. } => counts.cites += 1,
            InlineKind::Bold(inner)
            | InlineKind::Italic(inner)
            | InlineKind::Strikethrough(inner) => {
                count_inline(inner, counts);
            }
            InlineKind::Text(_) | InlineKind::Code(_) | InlineKind::FootnoteRef(_) => {}
        }
    }
}

/// Recursive walk collecting every `*.md` file, sorted for determinism.
fn walk(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                if path.file_name().is_some_and(|n| n != ".git") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn sql_err(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

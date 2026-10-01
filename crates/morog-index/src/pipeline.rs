//! The indexing pipeline (plan §3.2):
//!
//! ```text
//! walk (ignore crate; *.md|*.morg; skip .git)        [scanner: this thread]
//!   -> stat; compare (mtime_ns, size) with files table; changed -> blake3
//!   -> parse + extract FileFacts                     [rayon pool]
//!   -> bounded channel
//!   -> batch upsert, 256 files per BEGIN IMMEDIATE   [writer thread]
//! ```
//!
//! Full rebuilds (`--full`) are atomic: they build `index.sqlite.tmp` next to
//! the live database and `rename(2)` it over the old file (plan §3.1
//! principle 5). Incremental runs update in place; files present in the
//! table but missing on disk are purged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant, UNIX_EPOCH};

use ignore::WalkBuilder;
use rayon::prelude::*;
use rusqlite::{Connection, TransactionBehavior, params};

use crate::store::{delete_file_rows, store_file_facts};
use crate::{Error, schema};

/// Files per `BEGIN IMMEDIATE` transaction (plan §3.2).
const BATCH_SIZE: usize = 256;
/// Bound on the parse → writer channel, in files.
const CHANNEL_BOUND: usize = 512;

/// Counters for one indexing run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexStats {
    /// Note files seen by the walk.
    pub scanned: usize,
    /// Files whose (mtime_ns, size) differed from the table (or were new).
    pub changed: usize,
    /// Files actually re-parsed and written (hash really changed).
    pub indexed: usize,
    /// Rows purged for files no longer on disk.
    pub deleted: usize,
    /// Bibliography entries written to `bib_entries` (0 without a store).
    pub bib_entries: usize,
    /// Wall time of the whole run.
    pub wall: Duration,
}

/// A walked note file, before change detection.
struct Walked {
    abs: PathBuf,
    rel: String,
    mtime_ns: i64,
    size: i64,
}

/// A file that failed the stat compare and goes to the rayon pool.
struct Job {
    abs: PathBuf,
    rel: String,
    mtime_ns: i64,
    size: i64,
    prior: Option<(i64, Vec<u8>)>,
}

/// What the pool sends the writer for one file.
enum Msg {
    /// Stat changed but content hash did not: refresh stat columns only.
    Stat {
        file_id: i64,
        mtime_ns: i64,
        size: i64,
    },
    /// Content changed (or new file): replace all rows.
    File(Box<FileMsg>),
}

struct FileMsg {
    rel: String,
    mtime_ns: i64,
    size: i64,
    hash: [u8; 32],
    prior_file_id: Option<i64>,
    source: String,
    facts: morog_sema::FileFacts,
}

/// Index `vault_root` into the database at `db_path`, with no bibliography
/// (the `bib_entries` table is cleared). See [`index_vault_with_bib`].
pub fn index_vault(vault_root: &Path, db_path: &Path, full: bool) -> Result<IndexStats, Error> {
    index_vault_with_bib(vault_root, db_path, full, None)
}

/// Index `vault_root` into the database at `db_path`.
///
/// When `bib` is supplied, its entries replace the `bib_entries` table in
/// the same run (plan §4.2 — key completion and `v_dangling` never touch
/// the filesystem). `None` clears the table, keeping the index a pure
/// function of its inputs.
pub fn index_vault_with_bib(
    vault_root: &Path,
    db_path: &Path,
    full: bool,
    bib: Option<&dyn morog_bib::BibStore>,
) -> Result<IndexStats, Error> {
    let t0 = Instant::now();
    let vault = vault_root.canonicalize()?;
    let bib_entries = bib.map(|store| store.entries()).unwrap_or_default();

    let mut stats = if full {
        index_full(&vault, db_path, &bib_entries)?
    } else {
        let (conn, _fresh) = schema::open_rw(db_path)?;
        let existing = load_existing(&conn)?;
        let (stats, mut conn) = run_pipeline(conn, &vault, existing)?;
        write_bib(&mut conn, &bib_entries)?;
        drop(conn);
        stats
    };

    stats.bib_entries = bib_entries.len();
    stats.wall = t0.elapsed();
    Ok(stats)
}

/// Replace `bib_entries` in one transaction.
fn write_bib(conn: &mut Connection, entries: &[&morog_bib::BibEntry]) -> Result<(), Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    crate::store::store_bib_entries(&tx, entries)?;
    tx.commit()?;
    Ok(())
}

/// Full rebuild into `<db>.tmp`, then rename over the live file.
fn index_full(
    vault: &Path,
    db_path: &Path,
    bib_entries: &[&morog_bib::BibEntry],
) -> Result<IndexStats, Error> {
    let tmp = tmp_path(db_path);
    schema::remove_db(&tmp)?;
    if let Some(dir) = tmp.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(&tmp)?;
    schema::create_schema_bulk(&conn)?;

    let (stats, mut conn) = run_pipeline(conn, vault, HashMap::new())?;
    write_bib(&mut conn, bib_entries)?;
    schema::finish_bulk(&conn)?;
    drop(conn);

    // Drop stale WAL sidecars of the old database, then swap atomically.
    for suffix in ["-wal", "-shm"] {
        let mut os = db_path.as_os_str().to_owned();
        os.push(suffix);
        let _ = std::fs::remove_file(Path::new(&os));
    }
    std::fs::rename(&tmp, db_path)?;
    Ok(stats)
}

fn tmp_path(db_path: &Path) -> PathBuf {
    let mut os = db_path.as_os_str().to_owned();
    os.push(".tmp");
    PathBuf::from(os)
}

/// `files` table snapshot: path → (file_id, mtime_ns, size, hash).
type Existing = HashMap<String, (i64, i64, i64, Option<Vec<u8>>)>;

fn load_existing(conn: &Connection) -> Result<Existing, Error> {
    let mut stmt = conn.prepare("SELECT path, file_id, mtime_ns, size, hash FROM files")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?),
        ))
    })?;
    let mut map = Existing::new();
    for row in rows {
        let (path, value) = row?;
        map.insert(path, value);
    }
    Ok(map)
}

/// Walk, diff, parse, and write. Consumes and returns the connection so the
/// caller can finalize it (checkpoint, rename).
fn run_pipeline(
    conn: Connection,
    vault: &Path,
    mut existing: Existing,
) -> Result<(IndexStats, Connection), Error> {
    let walked = walk(vault)?;
    let scanned = walked.len();

    let mut jobs = Vec::new();
    for w in walked {
        match existing.remove(&w.rel) {
            Some((_, mtime_ns, size, _)) if mtime_ns == w.mtime_ns && size == w.size => {}
            prior => jobs.push(Job {
                abs: w.abs,
                rel: w.rel,
                mtime_ns: w.mtime_ns,
                size: w.size,
                prior: prior.map(|(id, _, _, hash)| (id, hash.unwrap_or_default())),
            }),
        }
    }
    let changed = jobs.len();
    // Whatever is left in the table was not found on disk: purge it.
    let doomed: Vec<i64> = existing.into_values().map(|(id, _, _, _)| id).collect();
    let deleted = doomed.len();

    let (tx, rx) = sync_channel::<Msg>(CHANNEL_BOUND);
    let vault_meta = vault.display().to_string();
    let writer = std::thread::spawn(move || -> Result<(usize, Connection), Error> {
        write_all(conn, rx, doomed, &vault_meta)
    });

    jobs.par_iter().for_each_with(tx, |tx, job| {
        match process(job) {
            Ok(msg) => {
                // A send error means the writer is gone; its own error wins.
                let _ = tx.send(msg);
            }
            Err(e) => eprintln!("morog index: skipping {}: {e}", job.abs.display()),
        }
    });
    // `for_each_with` moved and dropped all sender clones: the writer drains
    // the channel and finishes.

    let (indexed, conn) = writer
        .join()
        .map_err(|_| Error::Corrupt("index writer thread panicked".into()))??;

    Ok((
        IndexStats {
            scanned,
            changed,
            indexed,
            deleted,
            bib_entries: 0,
            wall: Duration::ZERO,
        },
        conn,
    ))
}

/// Walk the vault for `*.md` / `*.morg` files with the `ignore` crate.
///
/// `.git` and other dotfiles are skipped (`hidden`), and the vault's own
/// ignore files are honored — but parent directories' are not
/// (`parents(false)`), so a vault inside some larger ignored tree still
/// indexes, and no git repository is required (`require_git(false)`).
fn walk(vault: &Path) -> Result<Vec<Walked>, Error> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(vault)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false)
        .build();
    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                eprintln!("morog index: walk: {e}");
                continue;
            }
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if !path
            .extension()
            .is_some_and(|ext| ext == "md" || ext == "morg")
        {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(e) => {
                eprintln!("morog index: stat {}: {e}", path.display());
                continue;
            }
        };
        let rel = path
            .strip_prefix(vault)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        out.push(Walked {
            abs: path.to_path_buf(),
            rel,
            mtime_ns: mtime_ns(&meta),
            size: meta.len() as i64,
        });
    }
    Ok(out)
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Rayon-pool work for one changed file: read, hash, and — when the hash
/// really differs — parse and extract.
fn process(job: &Job) -> std::io::Result<Msg> {
    let raw = std::fs::read(&job.abs)?;
    let hash = blake3::hash(&raw);
    if let Some((file_id, prior_hash)) = &job.prior
        && prior_hash.as_slice() == hash.as_bytes()
    {
        return Ok(Msg::Stat {
            file_id: *file_id,
            mtime_ns: job.mtime_ns,
            size: job.size,
        });
    }
    let source = String::from_utf8_lossy(&raw).into_owned();
    let parsed = morg_parser::parse_document(&source);
    let analysis = morog_sema::analyze_document(&parsed.document);
    Ok(Msg::File(Box::new(FileMsg {
        rel: job.rel.clone(),
        mtime_ns: job.mtime_ns,
        size: job.size,
        hash: *hash.as_bytes(),
        prior_file_id: job.prior.as_ref().map(|(id, _)| *id),
        source,
        facts: analysis.facts,
    })))
}

/// Writer thread: batches of [`BATCH_SIZE`] files per `BEGIN IMMEDIATE`
/// transaction, then purges deleted files and stamps `meta`.
fn write_all(
    mut conn: Connection,
    rx: Receiver<Msg>,
    doomed: Vec<i64>,
    vault_root: &str,
) -> Result<(usize, Connection), Error> {
    let mut indexed = 0usize;
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    loop {
        let done = match rx.recv() {
            Ok(msg) => {
                batch.push(msg);
                false
            }
            Err(_) => true,
        };
        if batch.len() >= BATCH_SIZE || (done && !batch.is_empty()) {
            indexed += write_batch(&mut conn, &batch)?;
            batch.clear();
        }
        if done {
            break;
        }
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for file_id in doomed {
        delete_file_rows(&tx, file_id)?;
    }
    tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")?
        .execute(params!["vault_root", vault_root])?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")?
        .execute(params!["indexed_at_ms", now_ms.to_string()])?;
    tx.commit()?;
    Ok((indexed, conn))
}

fn write_batch(conn: &mut Connection, batch: &[Msg]) -> Result<usize, Error> {
    let mut indexed = 0;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for msg in batch {
        match msg {
            Msg::Stat {
                file_id,
                mtime_ns,
                size,
            } => {
                tx.prepare_cached("UPDATE files SET mtime_ns = ?1, size = ?2 WHERE file_id = ?3")?
                    .execute(params![mtime_ns, size, file_id])?;
            }
            Msg::File(file) => {
                if let Some(file_id) = file.prior_file_id {
                    delete_file_rows(&tx, file_id)?;
                }
                tx.prepare_cached(
                    "INSERT INTO files (path, mtime_ns, size, hash) VALUES (?1, ?2, ?3, ?4)",
                )?
                .execute(params![
                    file.rel,
                    file.mtime_ns,
                    file.size,
                    file.hash.as_slice()
                ])?;
                let file_id = tx.last_insert_rowid();
                store_file_facts(&tx, file_id, &file.source, &file.facts)?;
                indexed += 1;
            }
        }
    }
    tx.commit()?;
    Ok(indexed)
}

//! The embedded indexer (plan §5.1, §5.4): a background thread running the
//! `morog-index` incremental pipeline — the **only** writer to SQLite —
//! which reconciles the disk against the index at startup and on every
//! save / watched change, streaming resulting facts back to the main
//! thread.
//!
//! Open buffers always win: the main thread ignores indexer facts for files
//! with an overlay (plan §5.3).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use morog_bib::BibStore;
use morog_sema::FileFacts;

use crate::analysis::{BibIndex, FileAnalysis};
use crate::state::{GlobalState, Task};

/// What the indexer thread reports back to the main loop.
pub(crate) enum IndexEvent {
    /// Reconcile started (surfaced as `window/workDoneProgress`).
    Begin,
    /// Reconcile finished: the complete post-reconcile fact set (absolute
    /// paths) and the bibliography table. Open buffers are skipped on apply.
    Reloaded {
        files: Vec<(PathBuf, FileFacts)>,
        bib: Vec<morog_bib::BibEntry>,
        /// The bibrat root `TAGS.yml` vocabulary (not persisted in SQLite).
        tags: Vec<String>,
        summary: String,
    },
    /// One file re-indexed after a save or watched change; `None` facts
    /// mean the file is gone from the index (deleted on disk).
    Updated {
        path: PathBuf,
        facts: Option<Box<FileFacts>>,
    },
}

/// Main-thread handle: forward changed paths; the thread exits when the
/// handle (and its channel) drops with the `GlobalState`.
pub(crate) struct IndexerHandle {
    tx: Sender<PathBuf>,
}

impl IndexerHandle {
    pub fn notify_changed(&self, path: PathBuf) {
        let _ = self.tx.send(path);
    }
}

/// Spawn the indexer over `vault_root` (already canonicalized).
pub(crate) fn spawn(
    vault_root: PathBuf,
    bibrat_root: Option<PathBuf>,
    task_tx: Sender<Task>,
) -> IndexerHandle {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("morog-indexer".to_owned())
        .spawn(move || run(vault_root, bibrat_root, &rx, &task_tx))
        .expect("spawn indexer thread");
    IndexerHandle { tx }
}

fn send(task_tx: &Sender<Task>, event: IndexEvent) {
    let _ = task_tx.send(Task::Index(event));
}

fn run(
    vault_root: PathBuf,
    bibrat_root: Option<PathBuf>,
    rx: &Receiver<PathBuf>,
    task_tx: &Sender<Task>,
) {
    let db_path = morog_index::cache_db_path(&vault_root);

    // bibrat is read once per reconcile; discovery failure just means the
    // bibliography-backed behavior is skipped (plan §4).
    let bib = match &bibrat_root {
        Some(root) => morog_bib::BibratStore::open(root.clone()).ok(),
        None => morog_bib::BibratStore::discover_default().unwrap_or_default(),
    };
    let bib_store: Option<&dyn BibStore> = bib.as_ref().map(|b| b as &dyn BibStore);

    // Startup reconcile: stat-compare the disk against the index, then
    // stream the post-reconcile state back (plan §5.4 step 3).
    send(task_tx, IndexEvent::Begin);
    let summary = match morog_index::index_vault_with_bib(&vault_root, &db_path, false, bib_store) {
        Ok(stats) => format!(
            "indexed {} of {} files in {:.1?}",
            stats.indexed, stats.scanned, stats.wall
        ),
        Err(e) => format!("index error: {e}"),
    };
    let tags = bib.as_ref().map(BibStore::tags).unwrap_or_default();
    match load_everything(&db_path, &vault_root) {
        Ok((files, bib_entries)) => send(
            task_tx,
            IndexEvent::Reloaded {
                files,
                bib: bib_entries,
                tags,
                summary,
            },
        ),
        Err(e) => send(
            task_tx,
            IndexEvent::Reloaded {
                files: Vec::new(),
                bib: Vec::new(),
                tags: Vec::new(),
                summary: format!("index load error: {e}"),
            },
        ),
    }

    // Save / watched-change loop: coalesce bursts, re-run the incremental
    // pipeline (it only re-reads files whose stat changed), then report the
    // touched files (plan §5.3).
    while let Ok(first) = rx.recv() {
        let mut touched = vec![first];
        let settle = Instant::now() + Duration::from_millis(50);
        while let Ok(more) = rx.recv_deadline(settle) {
            touched.push(more);
        }
        if let Err(e) = morog_index::index_vault_with_bib(&vault_root, &db_path, false, bib_store) {
            eprintln!("morog-lsp: indexer: {e}");
            continue;
        }
        let Ok(conn) = morog_index::schema::open_ro(&db_path) else {
            continue;
        };
        touched.sort_unstable();
        touched.dedup();
        for path in touched {
            let Ok(rel) = path.strip_prefix(&vault_root) else {
                continue;
            };
            let facts = morog_index::load_facts_for_path(&conn, &rel.to_string_lossy())
                .ok()
                .flatten();
            send(
                task_tx,
                IndexEvent::Updated {
                    path,
                    facts: facts.map(Box::new),
                },
            );
        }
    }
}

/// Everything a reconcile streams back: per-file facts (absolute paths)
/// plus the bibliography.
type Loaded = (Vec<(PathBuf, FileFacts)>, Vec<morog_bib::BibEntry>);

/// One read-only table scan of the whole index.
fn load_everything(db_path: &Path, vault_root: &Path) -> Result<Loaded, morog_index::Error> {
    let conn = morog_index::schema::open_ro(db_path)?;
    let files = morog_index::load_all_facts(&conn)?
        .into_iter()
        .map(|(rel, facts)| (vault_root.join(rel), facts))
        .collect();
    let bib = morog_index::load_bib_entries(&conn)?;
    Ok((files, bib))
}

/// Apply one indexer event on the main thread.
pub(crate) fn apply_event(state: &mut GlobalState, event: IndexEvent) {
    match event {
        IndexEvent::Begin => state.index_progress_begin(),
        IndexEvent::Reloaded {
            files,
            bib,
            tags,
            summary,
        } => {
            state.analysis.bib = Arc::new(BibIndex::from_entries(bib));
            state.analysis.bib_tags = tags.into();
            for (path, facts) in files {
                state.set_indexed_file(&path, FileAnalysis::from_facts(facts));
            }
            state.analysis.generation += 1;
            // The bibliography may have (dis)appeared: the citekey lints of
            // open buffers depend on it, so reparse them against it.
            state.reanalyze_open_files();
            state.index_progress_end(summary);
        }
        IndexEvent::Updated { path, facts } => match facts {
            Some(facts) => state.set_indexed_file(&path, FileAnalysis::from_facts(*facts)),
            None => state.remove_indexed_file(&path),
        },
    }
}

//! `morog index` — build or refresh the disposable SQLite index
//! (plan §3.2), optionally watching the vault for changes.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use morg_parser::TagTable;
use morog_bib::{BibStore, BibratStore};
use morog_index::IndexStats;

use crate::cli::BibArgs;

/// How long to let filesystem events settle before re-indexing.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(300);

pub fn run(
    vault: Option<PathBuf>,
    full: bool,
    watch: bool,
    format: &str,
    bib_args: &BibArgs,
) -> ExitCode {
    let vault = vault.unwrap_or_else(|| PathBuf::from("."));
    let vault = match vault.canonicalize() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("morog index: cannot open vault {}: {e}", vault.display());
            return ExitCode::FAILURE;
        }
    };
    let db_path = morog_index::cache_db_path(&vault);

    // Missing store is a notice, not an error (plan §4): the index still
    // builds, with an empty `bib_entries` table.
    let store = match crate::bib::load(bib_args, "index") {
        Ok(store) => store,
        Err(message) => {
            eprintln!("morog index: {message}");
            return ExitCode::FAILURE;
        }
    };
    if store.is_none() && !bib_args.no_bib {
        eprintln!("morog index: no bibrat store found; bib_entries left empty");
    }
    let bib = store.as_ref().map(|s| s as &dyn BibStore);

    // Parse against the resolved `[tags]` table (plan §10 T1) so custom-tag
    // facts land in `blocks.customs` and the cache is keyed on the table.
    let tags = match crate::config::load(&vault) {
        Ok(config) => config.tag_table(),
        Err(message) => {
            eprintln!("morog index: {message}");
            return ExitCode::FAILURE;
        }
    };

    match morog_index::index_vault_with_tags(&vault, &db_path, full, bib, &tags) {
        Ok(stats) => report(&vault, &stats, format),
        Err(e) => {
            eprintln!("morog index: {e}");
            return ExitCode::FAILURE;
        }
    }

    if !watch {
        return ExitCode::SUCCESS;
    }
    if let Err(e) = watch_loop(&vault, &db_path, format, store.as_ref(), &tags) {
        eprintln!("morog index: watch: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn report(vault: &Path, stats: &IndexStats, format: &str) {
    if format == "json" {
        println!(
            "{{\"vault\": \"{}\", \"scanned\": {}, \"changed\": {}, \"indexed\": {}, \
             \"deleted\": {}, \"bib_entries\": {}, \"wall_ms\": {}}}",
            crate::json::escape(&vault.display().to_string()),
            stats.scanned,
            stats.changed,
            stats.indexed,
            stats.deleted,
            stats.bib_entries,
            stats.wall.as_millis()
        );
    } else {
        println!(
            "indexed {}: {} scanned, {} changed, {} indexed, {} deleted, {} bib in {:.2?}",
            vault.display(),
            stats.scanned,
            stats.changed,
            stats.indexed,
            stats.deleted,
            stats.bib_entries,
            stats.wall
        );
    }
}

/// Debounced watch loop: any event touching a note file (or something
/// without an extension, e.g. a directory) triggers an incremental rescan.
fn watch_loop(
    vault: &Path,
    db_path: &Path,
    format: &str,
    store: Option<&BibratStore>,
    tags: &TagTable,
) -> notify::Result<()> {
    use notify::{RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(vault, RecursiveMode::Recursive)?;
    eprintln!("watching {} (ctrl-c to stop)", vault.display());

    loop {
        // Block for the first event, then let the burst settle.
        let mut relevant = is_relevant(&rx.recv().map_err(disconnected)?);
        let deadline = std::time::Instant::now() + WATCH_DEBOUNCE;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(left) {
                Ok(event) => relevant |= is_relevant(&event),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(disconnected(std::sync::mpsc::RecvError));
                }
            }
        }
        if !relevant {
            continue;
        }
        // The bib and tag-table snapshots from startup are reused; only the
        // vault is watched.
        let bib = store.map(|s| s as &dyn BibStore);
        match morog_index::index_vault_with_tags(vault, db_path, false, bib, tags) {
            Ok(stats) if stats.changed == 0 && stats.deleted == 0 => {}
            Ok(stats) => report(vault, &stats, format),
            Err(e) => eprintln!("morog index: {e}"),
        }
    }
}

fn disconnected(_: std::sync::mpsc::RecvError) -> notify::Error {
    notify::Error::generic("watcher channel disconnected")
}

fn is_relevant(event: &notify::Result<notify::Event>) -> bool {
    let Ok(event) = event else {
        // Watcher errors (overflow, …) force a rescan to stay correct.
        return true;
    };
    event.paths.iter().any(|p| match p.extension() {
        Some(ext) => ext == "md" || ext == "morg",
        // Renames and removals of directories arrive without extensions.
        None => true,
    })
}

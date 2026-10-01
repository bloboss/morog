//! Shared bibrat-store loading for the `lint` and `index` subcommands
//! (`--bib` / `--no-bib` / `--bib-root`, see [`BibArgs`]).

use morog_bib::{BibStore, BibratStore};

use crate::cli::BibArgs;

/// Load the bibrat store per the flags. `Ok(None)` means "run without a
/// bibliography" — either requested (`--no-bib`) or because auto-discovery
/// found nothing. `Err` carries a user-facing message (explicit `--bib` /
/// `--bib-root` failures).
pub fn load(args: &BibArgs, cmd: &str) -> Result<Option<BibratStore>, String> {
    if args.no_bib {
        return Ok(None);
    }
    let store = if let Some(root) = &args.bib_root {
        Some(BibratStore::open(root).map_err(|e| e.to_string())?)
    } else {
        match BibratStore::discover_default() {
            Ok(store) => store,
            Err(e) if args.bib => return Err(e.to_string()),
            Err(e) => {
                eprintln!("morog {cmd}: bibrat store unusable, continuing without it: {e}");
                None
            }
        }
    };
    if args.bib && store.is_none() {
        return Err(
            "no bibrat store found (config [store] data_dir, $BIBRAT_DATA_DIR, \
             $XDG_DATA_HOME/bibrat, ~/.local/share/bibrat)"
                .into(),
        );
    }
    if let Some(store) = &store {
        report_warnings(store, cmd);
    }
    Ok(store)
}

/// Surface store- and entry-level warnings on stderr; they are advisory
/// and never affect the exit code.
fn report_warnings(store: &BibratStore, cmd: &str) {
    for warning in store.warnings() {
        eprintln!("morog {cmd}: bib: {warning}");
    }
    for entry in store.entries() {
        for warning in &entry.warnings {
            eprintln!("morog {cmd}: bib: {}: {warning}", entry.key.0);
        }
    }
}

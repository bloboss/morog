//! Smoke test over the generated benchmark vault (`bench/vault/`,
//! gitignored; regenerate with `cargo run --release -p morog-bench -- gen`).
//!
//! Parses and extracts every note and runs both lint passes, asserting the
//! pipeline survives a realistic 15k-file vault. Skips gracefully when the
//! vault has not been generated, so CI stays green.

use std::path::{Path, PathBuf};
use std::time::Instant;

use morog_sema::{LintConfig, NoteKind, Severity, VaultNote, analyze_document};

fn collect_notes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_notes(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "md" || ext == "morg")
        {
            out.push(path);
        }
    }
}

#[test]
fn bench_vault_smoke() {
    let vault = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/vault");
    if !vault.is_dir() {
        eprintln!("bench/vault not generated; skipping smoke test");
        return;
    }

    let t0 = Instant::now();
    let mut files = Vec::new();
    collect_notes(&vault, &mut files);
    assert!(
        files.len() > 1000,
        "suspiciously small bench vault: {} files",
        files.len()
    );

    let mut analyses = Vec::with_capacity(files.len());
    let mut file_diags = 0usize;
    let mut parse_errors = 0usize;
    let config = LintConfig::default();
    let mut kinds = [0usize; 3];

    for path in &files {
        let source = std::fs::read_to_string(path).expect("read note");
        let parsed = morg_parser::parse_document(&source);
        parse_errors += parsed.errors.len();
        let analysis = analyze_document(&source, &parsed.document);
        assert!(
            analysis.meta_errors.is_empty(),
            "unexpected meta errors in {}: {:?}",
            path.display(),
            analysis.meta_errors
        );
        match analysis.facts.note.kind {
            Some(NoteKind::Source) => kinds[0] += 1,
            Some(NoteKind::Topic) => kinds[1] += 1,
            Some(NoteKind::Argument) => kinds[2] += 1,
            None => panic!("bench note without kind: {}", path.display()),
        }
        file_diags += morog_sema::lint_file(path, &analysis, &config, None).len();
        analyses.push((path.clone(), analysis));
    }

    let notes: Vec<VaultNote<'_>> = analyses
        .iter()
        .map(|(path, a)| VaultNote {
            path,
            facts: &a.facts,
        })
        .collect();
    let vault_diags = morog_sema::lint_vault(&notes);
    // The generator plans the id space up front and tolerates a few unused
    // ids at the tail, so dangling relation targets are expected — but ids
    // are unique and no positional `#bN` address is ever emitted.
    use morog_sema::LintCode;
    assert!(!vault_diags.iter().any(|d| d.code == LintCode::Id));
    assert!(
        !vault_diags
            .iter()
            .any(|d| d.code == LintCode::PositionalAddress)
    );
    let vault_errors = vault_diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();

    eprintln!(
        "bench smoke: {} files ({} sources, {} topics, {} arguments), {} parse errors, {} file diagnostics, {} vault diagnostics ({} errors, all dangling targets) in {:.1}s",
        files.len(),
        kinds[0],
        kinds[1],
        kinds[2],
        parse_errors,
        file_diags,
        vault_diags.len(),
        vault_errors,
        t0.elapsed().as_secs_f64()
    );
}

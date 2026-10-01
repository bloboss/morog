//! `morog lint` — walk a vault, run the file-level and vault-level STA
//! lints (plan §2.5), and print diagnostics.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use morog_bib::BibStore;
use morog_sema::{Analysis, Diagnostic, LintConfig, Severity, VaultNote};

use crate::cli::BibArgs;

/// Run the lint subcommand over the given paths (default: the current
/// directory). Returns a failure exit code when any error-severity
/// diagnostic fires.
///
/// A discovered (or `--bib-root`-supplied) bibrat store enables the
/// bibliography-backed lints: `sta::unknown-citekey` and the
/// source-id-matches-key half of `sta::id`.
pub fn run(paths: Vec<PathBuf>, format: &str, bib_args: &BibArgs) -> ExitCode {
    let store = match crate::bib::load(bib_args, "lint") {
        Ok(store) => store,
        Err(message) => {
            eprintln!("morog lint: {message}");
            return ExitCode::FAILURE;
        }
    };
    let bib = store.as_ref().map(|s| s as &dyn BibStore);

    let roots = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths
    };

    let mut files = Vec::new();
    for root in &roots {
        if root.is_dir() {
            collect_notes(root, &mut files);
        } else {
            files.push(root.clone());
        }
    }
    files.sort();
    files.dedup();

    let config = LintConfig::default();
    let mut diagnostics = Vec::new();
    let mut analyses: Vec<(PathBuf, Analysis)> = Vec::with_capacity(files.len());

    for path in files {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(err) => {
                eprintln!("morog lint: cannot read {}: {err}", path.display());
                continue;
            }
        };
        let parsed = morg_parser::parse_document(&source);
        let analysis = morog_sema::analyze_document(&source, &parsed.document);
        diagnostics.extend(morog_sema::lint_file(&path, &analysis, &config, bib));
        analyses.push((path, analysis));
    }

    let notes: Vec<VaultNote<'_>> = analyses
        .iter()
        .map(|(path, analysis)| VaultNote {
            path,
            facts: &analysis.facts,
        })
        .collect();
    diagnostics.extend(morog_sema::lint_vault(&notes));

    diagnostics
        .sort_by(|a, b| (&a.file, a.line_col(), a.code).cmp(&(&b.file, b.line_col(), b.code)));

    match format {
        "json" => print!("{}", render_json(&diagnostics)),
        _ => print!("{}", render_text(&diagnostics, analyses.len())),
    }

    if diagnostics.iter().any(|d| d.severity == Severity::Error) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// Recursively collect `*.md` / `*.morg` files, skipping dot-entries.
fn collect_notes(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("morog lint: cannot read directory {}", dir.display());
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('.'))
        {
            continue;
        }
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

fn render_text(diagnostics: &[Diagnostic], file_count: usize) -> String {
    let mut out = String::new();
    for d in diagnostics {
        let (line, col) = d.line_col();
        let _ = writeln!(
            out,
            "{}:{line}:{col}: {}: {}",
            d.file.display(),
            d.code.as_str(),
            d.message
        );
    }
    let errors = diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = diagnostics.len() - errors;
    let _ = writeln!(
        out,
        "{} file(s) checked: {errors} error(s), {warnings} warning(s)",
        file_count
    );
    out
}

fn render_json(diagnostics: &[Diagnostic]) -> String {
    let mut out = String::from("[");
    for (i, d) in diagnostics.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let (line, col) = d.line_col();
        let _ = write!(
            out,
            "\n  {{\"file\": \"{}\", \"line\": {line}, \"col\": {col}, \"code\": \"{}\", \"severity\": \"{}\", \"message\": \"{}\"}}",
            crate::json::escape(&d.file.display().to_string()),
            d.code.as_str(),
            d.severity.as_str(),
            crate::json::escape(&d.message)
        );
    }
    if !diagnostics.is_empty() {
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

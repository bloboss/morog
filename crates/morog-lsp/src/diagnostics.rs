//! Diagnostics assembly and debounce policy (plan §5.1, §5.3).
//!
//! The published set for a file is the union of its file-local lints
//! (computed at parse time, stored on [`crate::analysis::FileAnalysis`]) and
//! the vault-level rules recomputed from the xref maps. Scheduling lives in
//! [`crate::state::GlobalState`]; the 150 ms debounce window is defined
//! here.

use std::time::Duration;

use morog_sema::Diagnostic;

use crate::analysis::Analysis;
use crate::vfs::FileId;

/// Keystroke → diagnostics debounce (plan §5.3 step 6).
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(150);

/// The full `sta::*` set for one file: local + cross-file.
pub(crate) fn full_set(analysis: &Analysis, file: FileId) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = analysis
        .file(file)
        .map(|fa| fa.local_diags.to_vec())
        .unwrap_or_default();
    out.extend(analysis.cross_diags(file));
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::analysis::FileAnalysis;

    #[test]
    fn full_set_unions_local_and_cross() {
        let mut analysis = Analysis::default();
        let path: Arc<Path> = Arc::from(Path::new("/v/arg.md").to_path_buf());
        // An argument with one dangling relation (cross-file) and too few
        // relations (local).
        let fa = Arc::new(FileAnalysis::parse(
            &path,
            "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: gone, stance: supports }\n---\n"
                .into(),
            &morog_sema::LintConfig::default(),
            None,
            &morg_parser::TagTable::empty(),
        ));
        let file = FileId(0);
        analysis.set_file(file, path, fa);

        let codes: Vec<&str> = full_set(&analysis, file)
            .iter()
            .map(|d| d.code.as_str())
            .collect();
        assert_eq!(codes, ["sta::argument-arity", "sta::relation-target"]);
    }
}

//! Per-file analysis: text, parse tree, facts, and file-local diagnostics
//! (plan §5.2).
//!
//! A [`FileAnalysis`] comes from one of two places:
//!
//! - [`FileAnalysis::parse`] — the full pipeline over source text
//!   (didOpen/didChange, or the indexer picking up a disk change). Text,
//!   `Document`, facts, and local lints are all present.
//! - [`FileAnalysis::from_facts`] — the cold-start path (plan §5.4): facts
//!   restored from the index with **no reparse**, so `text` and `doc` are
//!   `None` until the file is opened or changes on disk. Handlers that need
//!   source text fall back to reading the file lazily.

use std::path::Path;
use std::sync::Arc;

use morg_parser::{Document, LineIndex, TagTable};
use morog_bib::BibStore;
use morog_sema::{Diagnostic, FileFacts, LintConfig, lint_file};

/// Everything the server knows about one file.
#[derive(Debug, Clone)]
pub struct FileAnalysis {
    /// Source text; `None` for facts-only (cold-start) entries.
    pub text: Option<Arc<str>>,
    /// Parse tree; `None` for facts-only entries.
    pub doc: Option<Arc<Document>>,
    /// The extracted facts — always present, this is the unit the xref maps
    /// and the index agree on.
    pub facts: Arc<FileFacts>,
    /// File-local lint findings (`sta::*` minus the vault-level rules).
    /// Empty for facts-only entries until the file is first parsed.
    pub local_diags: Arc<[Diagnostic]>,
}

impl FileAnalysis {
    /// Parse `text` against the resolved user tag table (plan §10 T1) and
    /// run the file-local lints.
    pub fn parse(
        path: &Path,
        text: Arc<str>,
        config: &LintConfig,
        bib: Option<&dyn BibStore>,
        tags: &TagTable,
    ) -> Self {
        let parsed = morg_parser::parse_document_with(&text, tags);
        let analysis = morog_sema::analyze_document(&parsed.document);
        let local_diags = lint_file(path, &analysis, config, bib);
        Self {
            text: Some(text),
            doc: Some(Arc::new(parsed.document)),
            facts: Arc::new(analysis.facts),
            local_diags: local_diags.into(),
        }
    }

    /// Wrap facts restored from the index (plan §5.4 — no reparse).
    pub fn from_facts(facts: FileFacts) -> Self {
        Self {
            text: None,
            doc: None,
            facts: Arc::new(facts),
            local_diags: Arc::from([]),
        }
    }

    /// A line index over the text, when text is present.
    ///
    /// `LineIndex` borrows, so it is built on demand — one pass over a note
    /// file is far below any budget here.
    pub fn line_index(&self) -> Option<LineIndex<'_>> {
        self.text.as_deref().map(LineIndex::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOPIC: &str = "---\nid: sla\nkind: topic\ntitle: \"SLA\"\n---\n\nAll cited [@wright_leaf_2004]. #anchor evidence-1\n\nAn uncited paragraph.\n";

    #[test]
    fn parse_builds_facts_and_local_diags() {
        let fa = FileAnalysis::parse(
            Path::new("/vault/sla.md"),
            TOPIC.into(),
            &LintConfig::default(),
            None,
            &TagTable::empty(),
        );
        assert!(fa.text.is_some());
        assert!(fa.doc.is_some());
        assert_eq!(fa.facts.note.id.as_ref().unwrap().as_str(), "sla");
        assert_eq!(fa.facts.blocks.len(), 2);
        // The second block violates provenance.
        assert_eq!(fa.local_diags.len(), 1);
        assert_eq!(fa.local_diags[0].code, morog_sema::LintCode::UncitedBlock);
        assert!(fa.line_index().is_some());
    }

    #[test]
    fn from_facts_is_textless() {
        let parsed = FileAnalysis::parse(
            Path::new("/vault/sla.md"),
            TOPIC.into(),
            &LintConfig::default(),
            None,
            &TagTable::empty(),
        );
        let restored = FileAnalysis::from_facts((*parsed.facts).clone());
        assert!(restored.text.is_none());
        assert!(restored.doc.is_none());
        assert!(restored.line_index().is_none());
        assert_eq!(restored.facts, parsed.facts);
        assert!(restored.local_diags.is_empty());
    }
}

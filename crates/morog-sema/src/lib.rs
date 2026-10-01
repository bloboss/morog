//! Semantic layer of the morog research toolkit (plan §2, milestone M2).
//!
//! `morog-sema` sits directly on top of `morg-parser` and gives STA notes
//! their meaning:
//!
//! - **Typed STA frontmatter** ([`meta`]) — explicit, span-carrying
//!   conversion of the `source` / `topic` / `argument` schemas from the
//!   parser's `saphyr` value tree.
//! - **[`FileFacts`] extraction** ([`facts`]) — reducing one parsed document
//!   to the plain data facts the index stores: note identity, declared
//!   relations, per-block citations and anchors, and `id:` references.
//! - **Lints** ([`lint`]) — the provenance and structural checks of plan
//!   §2.5, reported with spans.

pub mod facts;
pub mod lint;
pub mod meta;
pub mod types;

pub use facts::{
    BlockFacts, BlockUnit, CiteRef, FileFacts, IdRef, NoteFacts, RelationFacts, extract_facts,
};
pub use lint::{Diagnostic, LintCode, LintConfig, Severity, VaultNote, lint_file, lint_vault};
pub use meta::{
    ArgumentMeta, Extra, MetaError, MetaErrorKind, NoteMeta, RelatedDecl, RelationDecl, SourceMeta,
    TopicMeta, parse_note_meta, split_target,
};
pub use types::{
    ArgumentForm, ArgumentStatus, BloomOp, CiteKey, NoteId, NoteKind, NoteUuid, RelationKind,
    SourceStatus, Stance, is_slug,
};

use morg_parser::Document;

/// The combined semantic view of one parsed document.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    /// Typed frontmatter (best effort when `meta_errors` is non-empty).
    pub meta: NoteMeta,
    /// Schema violations found during conversion, with spans.
    pub meta_errors: Vec<MetaError>,
    /// The extracted, index-ready facts.
    pub facts: FileFacts,
}

/// Type the frontmatter and extract the facts of one parsed document.
///
/// All diagnostics carry spans taken from the parser's node tree, so the
/// original source text is not needed here.
pub fn analyze_document(doc: &Document) -> Analysis {
    let (meta, meta_errors) = parse_note_meta(doc.frontmatter.as_ref());
    let facts = extract_facts(doc, &meta);
    Analysis {
        meta,
        meta_errors,
        facts,
    }
}

#[cfg(test)]
mod tests {
    /// Smoke test: the `morg-parser` path dependency parses a small document.
    #[test]
    fn morg_parser_smoke() {
        let source = "# A heading\n\nA paragraph of plain text.\n";
        let result = morg_parser::parse_document(source);
        assert!(result.errors.is_empty(), "unexpected parse errors");
        let content_blocks = result
            .document
            .children
            .iter()
            .filter(|block| !matches!(block, morg_parser::Block::BlankLine(_)))
            .count();
        assert_eq!(content_blocks, 2);
    }
}

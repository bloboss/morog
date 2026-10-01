//! Semantic layer of the morog research toolkit (plan §2, milestone M2).
//!
//! `morog-sema` sits directly on top of `morg-parser` and gives STA notes
//! their meaning:
//!
//! - **Typed STA frontmatter** ([`meta`]) — explicit, span-carrying
//!   conversion of the `source` / `topic` / `argument` schemas from the
//!   parser's `saphyr` value tree.
//! - **`FileFacts` extraction** and the **plan §2.5 lints** follow.

pub mod meta;
pub mod types;

pub use meta::{
    ArgumentMeta, Extra, MetaError, MetaErrorKind, NoteMeta, RelatedDecl, RelationDecl, SourceMeta,
    TopicMeta, parse_note_meta, split_target,
};
pub use types::{
    ArgumentForm, ArgumentStatus, BloomOp, CiteKey, NoteId, NoteKind, NoteUuid, RelationKind,
    SourceStatus, Stance, is_slug,
};

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

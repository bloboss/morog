//! Semantic layer of the morog research toolkit.
//!
//! `morog-sema` sits directly on top of `morg-parser` and gives STA notes
//! their meaning:
//!
//! - **Typed STA frontmatter** — deserializing and validating the strict
//!   `source` / `topic` / `argument` frontmatter schemas (ids, thesis,
//!   typed relations with stance and operation).
//! - **`FileFacts` extraction** — reducing one parsed document to the plain
//!   data facts the index stores: note kind, id, relations, citations, and
//!   block anchors. Extraction is pure (`&Document -> FileFacts`) so the
//!   index can rebuild from read-only vault access.
//! - **Lints** — provenance and structural checks (unsourced topic blocks,
//!   unknown citation keys, dangling relations) reported with spans.
//!
//! Everything below is a placeholder; no extraction or lint logic is
//! implemented yet.

/// The three strictly enforced STA note kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoteKind {
    /// Anchor to raw material: citation-shaped title, DOI/PDF, mostly empty.
    Source,
    /// Everything known about one narrow topic, every block sourced.
    Topic,
    /// Synthesis in the author's own words; carries the thesis.
    Argument,
}

/// The extracted, index-ready facts of a single vault file.
///
/// Placeholder: fields (kind, id, uuid, relations, citations, anchors) are
/// added as extraction is implemented.
pub struct FileFacts;

#[cfg(test)]
mod tests {
    /// Smoke test: the `morg-parser` path dependency parses a small document.
    ///
    /// Deliberately only counts top-level blocks — the inline-segment API is
    /// under concurrent refactor and must not be touched here.
    #[test]
    fn morg_parser_smoke() {
        let source = "# A heading\n\nA paragraph of plain text.\n";
        let result = morg_parser::parse_document(source);
        assert!(result.errors.is_empty(), "unexpected parse errors");
        // Heading and paragraph (blank-line blocks excluded).
        let content_blocks = result
            .document
            .children
            .iter()
            .filter(|block| !matches!(block, morg_parser::Block::BlankLine(_)))
            .count();
        assert_eq!(content_blocks, 2);
    }
}

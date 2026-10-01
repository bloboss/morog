//! morog types → LSP types (plan §5.4).
//!
//! All internal offsets are UTF-8 bytes; UTF-16 exists only here and in
//! `from_proto`, converted through `morg_parser::LineIndex` when the client
//! didn't accept `utf-8`.

use std::path::Path;

use lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range, Url};
use morg_parser::{LineIndex, Span};
use morog_sema::Severity;

use crate::PositionEncoding;

/// Byte offset → protocol position, clamped into the text.
pub(crate) fn position(
    text: &str,
    index: &LineIndex<'_>,
    offset: usize,
    encoding: PositionEncoding,
) -> Position {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_col = match encoding {
        PositionEncoding::Utf8 => index.line_col(offset),
        PositionEncoding::Utf16 => index.line_col_utf16(offset),
    };
    // Cannot fail after clamping, but degrade to the origin rather than
    // panic in a language server.
    line_col.map_or_else(
        || Position::new(0, 0),
        |lc| Position::new(lc.line - 1, lc.col - 1),
    )
}

/// Byte span → protocol range.
pub(crate) fn range(
    text: &str,
    index: &LineIndex<'_>,
    span: Span,
    encoding: PositionEncoding,
) -> Range {
    Range::new(
        position(text, index, span.start, encoding),
        position(text, index, span.end, encoding),
    )
}

/// A whole-file range fallback for spanless diagnostics: the first line.
fn first_line_range(text: &str) -> Range {
    let end = text.find('\n').unwrap_or(text.len()) as u32;
    Range::new(Position::new(0, 0), Position::new(0, end))
}

/// One lint finding → protocol diagnostic.
pub(crate) fn diagnostic(
    text: &str,
    index: &LineIndex<'_>,
    diag: &morog_sema::Diagnostic,
    encoding: PositionEncoding,
) -> lsp_types::Diagnostic {
    let range = match diag.span {
        Some(span) => range(text, index, span, encoding),
        None => first_line_range(text),
    };
    lsp_types::Diagnostic {
        range,
        severity: Some(match diag.severity {
            Severity::Error => DiagnosticSeverity::ERROR,
            Severity::Warning => DiagnosticSeverity::WARNING,
        }),
        code: Some(NumberOrString::String(diag.code.as_str().to_owned())),
        source: Some("morog".to_owned()),
        message: diag.message.clone(),
        ..Default::default()
    }
}

/// Path → file URI.
pub(crate) fn url(path: &Path) -> Option<Url> {
    Url::from_file_path(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_respect_the_negotiated_encoding() {
        // '🦀' is 4 UTF-8 bytes, 2 UTF-16 units.
        let text = "a🦀b\nnext";
        let index = LineIndex::new(text);
        let b_offset = 5;
        assert_eq!(
            position(text, &index, b_offset, PositionEncoding::Utf8),
            Position::new(0, 5)
        );
        assert_eq!(
            position(text, &index, b_offset, PositionEncoding::Utf16),
            Position::new(0, 3)
        );
        // Past EOF clamps; inside the crab clamps down to its start.
        assert_eq!(
            position(text, &index, 999, PositionEncoding::Utf8),
            Position::new(1, 4)
        );
        assert_eq!(
            position(text, &index, 2, PositionEncoding::Utf16),
            Position::new(0, 1)
        );
    }

    #[test]
    fn spanless_diagnostics_cover_the_first_line() {
        let text = "---\nid: x\n---\n";
        let index = LineIndex::new(text);
        let diag = morog_sema::Diagnostic::new(
            morog_sema::LintCode::TopicTooLong,
            "too long".into(),
            Path::new("/v/x.md"),
            None,
        );
        let lsp = diagnostic(text, &index, &diag, PositionEncoding::Utf16);
        assert_eq!(
            lsp.range,
            Range::new(Position::new(0, 0), Position::new(0, 3))
        );
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            lsp.code,
            Some(NumberOrString::String("sta::topic-too-long".into()))
        );
        assert_eq!(lsp.source.as_deref(), Some("morog"));
    }
}

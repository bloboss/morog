//! LSP types → morog types (plan §5.4).
//!
//! Protocol positions arrive in the negotiated encoding and leave here as
//! UTF-8 byte offsets. Positions past the end of a line clamp to the line
//! end and positions past the last line clamp to EOF, per the LSP spec's
//! leniency rules.

use std::path::PathBuf;

use lsp_types::{Position, TextDocumentContentChangeEvent, Url};
use morg_parser::{LineCol, LineIndex};

use crate::PositionEncoding;

/// File URI → filesystem path.
pub(crate) fn abs_path(uri: &Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

/// Protocol position → byte offset, with the spec's clamping behaviour.
pub(crate) fn offset(
    text: &str,
    index: &LineIndex<'_>,
    position: Position,
    encoding: PositionEncoding,
) -> usize {
    let line_col = LineCol {
        line: position.line + 1,
        col: position.character + 1,
    };
    let exact = match encoding {
        PositionEncoding::Utf8 => index.offset(line_col),
        PositionEncoding::Utf16 => index.offset_utf16(line_col),
    };
    if let Some(offset) = exact {
        return offset;
    }
    // Clamp: line past EOF → text end; column past EOL → line end.
    let Some(line_start) = index.offset(LineCol {
        line: line_col.line,
        col: 1,
    }) else {
        return text.len();
    };
    text[line_start..]
        .find('\n')
        .map_or(text.len(), |nl| line_start + nl)
}

/// Apply didChange content changes to `text` (INCREMENTAL sync, plan §5.3).
pub(crate) fn apply_changes(
    text: &str,
    changes: &[TextDocumentContentChangeEvent],
    encoding: PositionEncoding,
) -> String {
    let mut text = text.to_owned();
    for change in changes {
        match change.range {
            None => text = change.text.clone(),
            Some(range) => {
                let index = LineIndex::new(&text);
                let start = offset(&text, &index, range.start, encoding);
                let end = offset(&text, &index, range.end, encoding).max(start);
                text.replace_range(start..end, &change.text);
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(
        range: Option<((u32, u32), (u32, u32))>,
        text: &str,
    ) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: range.map(|((sl, sc), (el, ec))| {
                lsp_types::Range::new(Position::new(sl, sc), Position::new(el, ec))
            }),
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn incremental_edits_apply_in_order() {
        let text = "hello\nworld\n";
        let edited = apply_changes(
            text,
            &[
                change(Some(((1, 0), (1, 5))), "vault"),
                change(Some(((0, 5), (0, 5))), ","),
            ],
            PositionEncoding::Utf16,
        );
        assert_eq!(edited, "hello,\nvault\n");
    }

    #[test]
    fn full_replacement_wins() {
        let edited = apply_changes("old", &[change(None, "new")], PositionEncoding::Utf8);
        assert_eq!(edited, "new");
    }

    #[test]
    fn multibyte_edit_respects_encoding() {
        // Insert after the crab: UTF-16 column 3, UTF-8 column 5.
        let text = "a🦀b\n";
        let utf16 = apply_changes(
            text,
            &[change(Some(((0, 3), (0, 3))), "!")],
            PositionEncoding::Utf16,
        );
        assert_eq!(utf16, "a🦀!b\n");
        let utf8 = apply_changes(
            text,
            &[change(Some(((0, 5), (0, 5))), "!")],
            PositionEncoding::Utf8,
        );
        assert_eq!(utf8, "a🦀!b\n");
    }

    #[test]
    fn out_of_range_positions_clamp() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(
            offset(text, &index, Position::new(0, 99), PositionEncoding::Utf16),
            2
        );
        assert_eq!(
            offset(text, &index, Position::new(9, 0), PositionEncoding::Utf16),
            5
        );
    }
}

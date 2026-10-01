//! `textDocument/inlayHint` (plan §5.6): after `to: squares`, show the
//! target's title; after an `id#anchor` target, show the first words of the
//! addressed block.
//!
//! Hints cover the frontmatter relation targets (`relations:` and
//! `related:` — both are `to`-shaped); inline links already show their own
//! link text, so they get none.

use lsp_types::{InlayHint, InlayHintLabel, InlayHintParams};
use morg_parser::LineIndex;

use crate::handlers::{HandlerError, anchor_block, file_text};
use crate::state::Snapshot;
use crate::{from_proto, to_proto};

/// Word budget for the first-words-of-the-block label.
const LABEL_WORDS: usize = 6;

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: InlayHintParams,
) -> Result<Option<Vec<InlayHint>>, HandlerError> {
    let Some(file) = from_proto::abs_path(&params.text_document.uri)
        .and_then(|path| snapshot.analysis.file_id(&path))
    else {
        return Ok(None);
    };
    let Some(fa) = snapshot.analysis.file(file).cloned() else {
        return Ok(None);
    };
    let Some(text) = file_text(snapshot, file) else {
        return Ok(None);
    };
    let index = LineIndex::new(&text);
    let start = from_proto::offset(&text, &index, params.range.start, snapshot.encoding);
    let end = from_proto::offset(&text, &index, params.range.end, snapshot.encoding);

    let mut hints = Vec::new();
    for rel in &fa.facts.relations {
        if rel.span.end < start || rel.span.start > end {
            continue;
        }
        let Some(label) = target_label(snapshot, &rel.target_id, rel.target_anchor.as_deref())
        else {
            continue;
        };
        hints.push(InlayHint {
            position: to_proto::position(&text, &index, rel.span.end, snapshot.encoding),
            label: InlayHintLabel::String(label),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: Some(true),
            padding_right: None,
            data: None,
        });
    }
    Ok(Some(hints))
}

/// The hint text for one target: the anchored block's first words, or the
/// target note's title.
fn target_label(snapshot: &Snapshot, target_id: &str, anchor: Option<&str>) -> Option<String> {
    let file = snapshot.analysis.resolve_id(target_id)?;
    let fa = snapshot.analysis.file(file)?;
    match anchor {
        Some(anchor) => {
            let block = anchor_block(fa, anchor)?;
            let text = file_text(snapshot, file)?;
            let block_text = text.get(block.span.start..block.span.end)?;
            Some(first_words(block_text, LABEL_WORDS))
        }
        None => fa.facts.note.title.clone().filter(|t| !t.is_empty()),
    }
}

/// The first `n` whitespace-separated words, with an ellipsis when cut.
fn first_words(text: &str, n: usize) -> String {
    let mut words = text.split_whitespace();
    let mut out: Vec<&str> = words.by_ref().take(n).collect();
    let cut = words.next().is_some();
    if cut && let Some(last) = out.last_mut() {
        *last = last.trim_end_matches(|c: char| c.is_ascii_punctuation());
    }
    let mut out = out.join(" ");
    if cut {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{Position, Range, TextDocumentIdentifier, WorkDoneProgressParams};

    use super::*;
    use crate::handlers::test_support::{fmt_position, snapshot};

    fn check(files: &[(&str, &str)], expected: Expect) {
        let (snap, position) = snapshot(files, &["wright2004"]);
        let response = handle(
            &snap,
            InlayHintParams {
                text_document: TextDocumentIdentifier {
                    uri: position.text_document.uri,
                },
                range: Range::new(Position::new(0, 0), Position::new(999, 0)),
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
        .unwrap();
        let rendered = response.map_or("<none>".to_owned(), |hints| {
            hints
                .iter()
                .map(|hint| {
                    let label = match &hint.label {
                        InlayHintLabel::String(s) => s.as_str(),
                        other => panic!("unexpected label {other:?}"),
                    };
                    format!("{} `{label}`", fmt_position(hint.position))
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        expected.assert_eq(&rendered);
    }

    const SLA: &str = "---\nid: sla\nkind: topic\ntitle: \"Specific leaf area\"\n---\n\nSLA anchors the whole leaf economics spectrum [@wright2004]. #anchor e1\n";

    #[test]
    fn relation_targets_show_title_or_block_words() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/arg.md",
                    "---\nid: a$0rg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: supports }\n  - { to: sla#e1, stance: qualifies }\n  - { to: gone, stance: opposes }\n---\n",
                ),
            ],
            expect![[r#"
                5:13 `Specific leaf area`
                6:16 `SLA anchors the whole leaf economics…`"#]],
        );
    }

    #[test]
    fn related_entries_show_titles_too() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/lmr.md",
                    "---\nid: l$0mr\nkind: topic\nrelated: [sla]\n---\n",
                ),
            ],
            expect!["3:13 `Specific leaf area`"],
        );
    }

    #[test]
    fn untitled_targets_yield_no_hint() {
        check(
            &[
                ("/v/t.md", "---\nid: t\nkind: topic\n---\n"),
                (
                    "/v/lmr.md",
                    "---\nid: l$0mr\nkind: topic\nrelated: [t]\n---\n",
                ),
            ],
            expect![""],
        );
    }
}

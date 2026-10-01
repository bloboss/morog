//! `textDocument/hover` (plan §5.6): on an id — kind, title, and first
//! block (for arguments, the thesis); on `[@key]` — bibliography metadata;
//! on an `id#anchor` target — that block's text.

use lsp_types::{Hover, HoverContents, HoverParams, MarkupContent, MarkupKind};
use morg_parser::LineIndex;

use crate::analysis::cursor::{self, CursorContext};
use crate::handlers::{HandlerError, anchor_block, file_position, file_text};
use crate::state::Snapshot;
use crate::to_proto;

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: HoverParams,
) -> Result<Option<Hover>, HandlerError> {
    let Some(pos) = file_position(snapshot, &params.text_document_position_params) else {
        return Ok(None);
    };
    let context = cursor::classify(&pos.fa, pos.offset);
    let (markdown, span) = match &context {
        CursorContext::LinkTarget {
            target_id,
            anchor,
            span,
        } => {
            let Some(markdown) = note_hover(snapshot, target_id, anchor.as_deref()) else {
                return Ok(None);
            };
            (markdown, *span)
        }
        CursorContext::Cite { key, span } => {
            let Some(markdown) = cite_hover(snapshot, key) else {
                return Ok(None);
            };
            (markdown, *span)
        }
        CursorContext::IdDecl { id, span } => {
            let Some(markdown) = note_hover(snapshot, id, None) else {
                return Ok(None);
            };
            (markdown, *span)
        }
        _ => return Ok(None),
    };

    let index = LineIndex::new(&pos.text);
    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: Some(to_proto::range(&pos.text, &index, span, snapshot.encoding)),
    }))
}

/// Hover text for a note id (optionally one of its anchored blocks).
fn note_hover(snapshot: &Snapshot, id: &str, anchor: Option<&str>) -> Option<String> {
    let file = snapshot.analysis.resolve_id(id)?;
    let fa = snapshot.analysis.file(file)?;
    let note = &fa.facts.note;
    let kind = note.kind.map_or("note", |k| k.as_str());

    let mut markdown = match &note.title {
        Some(title) => format!("**{kind}** `{id}` — {title}"),
        None => format!("**{kind}** `{id}`"),
    };

    if let Some(anchor) = anchor {
        // An `id#anchor` target hovers the addressed block's text.
        let block = anchor_block(fa, anchor)?;
        let text = file_text(snapshot, file)?;
        let block_text = text.get(block.span.start..block.span.end)?.trim();
        markdown.push_str(&format!("\n\n`#{anchor}`\n\n{block_text}"));
        return Some(markdown);
    }

    // Arguments show their thesis; everything else its first block.
    let body = match &note.thesis {
        Some(thesis) => Some(format!("*{thesis}*")),
        None => fa.facts.blocks.first().and_then(|block| {
            let text = file_text(snapshot, file)?;
            Some(
                text.get(block.span.start..block.span.end)?
                    .trim()
                    .to_owned(),
            )
        }),
    };
    if let Some(body) = body
        && !body.is_empty()
    {
        markdown.push_str("\n\n");
        markdown.push_str(&body);
    }
    Some(markdown)
}

/// Hover text for a citation key: the bibliography entry (plan §4.2 —
/// virtual sources resolve even without a source note).
fn cite_hover(snapshot: &Snapshot, key: &str) -> Option<String> {
    let entry = snapshot.analysis.bib.get(key)?;
    let mut markdown = format!("**citation** `@{key}`");
    if let Some(meta) = &entry.meta {
        if let Some(title) = &meta.title {
            markdown.push_str(&format!(" — {title}"));
        }
        if let Some(year) = meta.year {
            markdown.push_str(&format!(" ({year})"));
        }
        if let Some(doi) = &meta.doi {
            markdown.push_str(&format!("\n\ndoi: `{doi}`"));
        }
    }
    if !entry.collection.0.is_empty() {
        markdown.push_str(&format!("\n\ncollection: {}", entry.collection));
    }
    Some(markdown)
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::WorkDoneProgressParams;

    use super::*;
    use crate::handlers::test_support::snapshot;

    fn check(files: &[(&str, &str)], bib: &[&str], expected: Expect) {
        let (snap, position) = snapshot(files, bib);
        let hover = handle(
            &snap,
            HoverParams {
                text_document_position_params: position,
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        )
        .unwrap();
        let rendered = hover.map_or("<none>".to_owned(), |h| match h.contents {
            HoverContents::Markup(m) => m.value,
            other => format!("{other:?}"),
        });
        expected.assert_eq(&rendered);
    }

    const SLA: &str = "---\nid: sla\nkind: topic\ntitle: \"Specific leaf area\"\n---\n\nSLA anchors the leaf economics spectrum [@wright2004]. #anchor e1\n";
    const ARG: &str = "---\nid: arg\nkind: argument\ntitle: \"Allocation in disguise\"\nthesis: \"It is all allocation.\"\nrelations:\n  - { to: sla, stance: supports }\n  - { to: sla#e1, stance: qualifies }\n---\n";

    #[test]
    fn hover_id_link_shows_kind_title_first_block() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: topic\n---\n\nSee [sla](id:s$0la). #mine\n",
                ),
            ],
            &["wright2004"],
            expect![[r#"
                **topic** `sla` — Specific leaf area

                SLA anchors the leaf economics spectrum [@wright2004]. #anchor e1"#]],
        );
    }

    #[test]
    fn hover_relation_target_on_argument_shows_thesis() {
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/arg.md", ARG),
                (
                    "/v/arg2.md",
                    "---\nid: arg2\nkind: argument\nthesis: \"x\"\nrelations:\n  - { to: a$0rg, stance: opposes }\n---\n",
                ),
            ],
            &["wright2004"],
            expect![[r#"
                **argument** `arg` — Allocation in disguise

                *It is all allocation.*"#]],
        );
    }

    #[test]
    fn hover_anchor_target_shows_block_text() {
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/arg.md", ARG.replacen("sla#e1", "sla#e$01", 1).leak()),
            ],
            &["wright2004"],
            expect![[r#"
                **topic** `sla` — Specific leaf area

                `#e1`

                SLA anchors the leaf economics spectrum [@wright2004]. #anchor e1"#]],
        );
    }

    #[test]
    fn hover_cite_shows_bib_metadata() {
        check(
            &[(
                "/v/sla.md",
                SLA.replacen("[@wright2004]", "[@wright$02004]", 1).leak(),
            )],
            &["wright2004"],
            expect![[r#"
                **citation** `@wright2004` — The wright2004 paper (2020)

                collection: fixtures"#]],
        );
    }

    #[test]
    fn hover_unknown_target_is_none() {
        check(
            &[(
                "/v/a.md",
                "---\nid: a\nkind: topic\n---\n\nSee [x](id:no$0pe). #mine\n",
            )],
            &[],
            expect!["<none>"],
        );
    }

    #[test]
    fn hover_prose_is_none() {
        check(
            &[(
                "/v/a.md",
                "---\nid: a\nkind: topic\n---\n\nPlain pr$0ose. #mine\n",
            )],
            &[],
            expect!["<none>"],
        );
    }
}

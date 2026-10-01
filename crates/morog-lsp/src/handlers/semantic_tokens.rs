//! `textDocument/semanticTokens/full` (plan §5.6): links coloured by the
//! **target's** kind, stance and op values in frontmatter, citekeys,
//! `#mine` tags, and the named-group fields of custom tags.
//!
//! The legend is deliberately minimal — seven token types, no modifiers:
//!
//! | index | type         | used for                                   |
//! |-------|--------------|--------------------------------------------|
//! | 0     | `class`      | link/relation whose target is a **topic**  |
//! | 1     | `function`   | link/relation whose target is an **argument** |
//! | 2     | `type`       | link/relation whose target is a **source** |
//! | 3     | `enumMember` | `stance:` and `op:` values in frontmatter  |
//! | 4     | `macro`      | inline `[@key]` citations                  |
//! | 5     | `decorator`  | `#mine` tags                               |
//! | 6     | `property`   | named-group field sub-spans inside custom tag arguments (plan §10 T1 — config-driven highlighting lands via LSP, not tree-sitter) |
//!
//! Unresolved targets stay uncoloured (the `sta::relation-target` diagnostic
//! already marks them). Content nested in callouts is skipped: its spans are
//! relative to the parser's reassembled buffer, not the file.

use lsp_types::{
    SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensLegend, SemanticTokensParams,
    SemanticTokensResult,
};
use morg_parser::{Block, InlineContent, InlineKind, LineIndex, Span, Tag, TagKind};
use morog_sema::NoteKind;

use crate::handlers::{HandlerError, file_text};
use crate::state::Snapshot;
use crate::{from_proto, to_proto};

const TOKEN_TOPIC: u32 = 0;
const TOKEN_ARGUMENT: u32 = 1;
const TOKEN_SOURCE: u32 = 2;
const TOKEN_STANCE_OP: u32 = 3;
const TOKEN_CITE: u32 = 4;
const TOKEN_MINE: u32 = 5;
const TOKEN_CUSTOM_FIELD: u32 = 6;

/// The legend advertised in the server capabilities; indices must match the
/// `TOKEN_*` constants above. Index 6 (`property`) is the plan §10 T1
/// addition: each named-group field of a matched custom tag argument gets
/// its sub-span (the capture's absolute byte range) coloured, so
/// config-declared structure is visible without any grammar change.
pub(crate) fn legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::CLASS,
            SemanticTokenType::FUNCTION,
            SemanticTokenType::TYPE,
            SemanticTokenType::ENUM_MEMBER,
            SemanticTokenType::MACRO,
            SemanticTokenType::new("decorator"),
            SemanticTokenType::PROPERTY,
        ],
        token_modifiers: Vec::new(),
    }
}

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: SemanticTokensParams,
) -> Result<Option<SemanticTokensResult>, HandlerError> {
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

    // Collect (span, token type) pairs from the facts and the parse tree.
    let mut raw: Vec<(Span, u32)> = Vec::new();

    let target_token = |target_id: &str| {
        let file = snapshot.analysis.resolve_id(target_id)?;
        match snapshot.analysis.file(file)?.facts.note.kind {
            Some(NoteKind::Topic) => Some(TOKEN_TOPIC),
            Some(NoteKind::Argument) => Some(TOKEN_ARGUMENT),
            Some(NoteKind::Source) => Some(TOKEN_SOURCE),
            None => None,
        }
    };
    for rel in &fa.facts.relations {
        if let Some(token) = target_token(&rel.target_id) {
            raw.push((rel.span, token));
        }
    }
    for id_ref in &fa.facts.id_refs {
        if let Some(token) = target_token(&id_ref.target_id) {
            raw.push((id_ref.span, token));
        }
    }
    for block in &fa.facts.blocks {
        for cite in &block.cites {
            raw.push((cite.span, TOKEN_CITE));
        }
    }
    if let Some(doc) = &fa.doc {
        if let Some(fm) = &doc.frontmatter
            && let Some(items) = fm.entry("relations").and_then(|entry| entry.value.items())
        {
            for item in items {
                for key in ["stance", "op"] {
                    if let Some(entry) = item.entry(key) {
                        raw.push((entry.value.span, TOKEN_STANCE_OP));
                    }
                }
            }
        }
        for block in &doc.children {
            tag_token_spans(block, &mut raw);
        }
    }

    raw.sort_by_key(|(span, _)| (span.start, span.end));
    raw.dedup();

    // Encode: absolute (line, start, length) in the negotiated encoding,
    // then delta-compress per the protocol.
    let index = LineIndex::new(&text);
    let mut data = Vec::with_capacity(raw.len());
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for (span, token_type) in raw {
        let start = to_proto::position(&text, &index, span.start, snapshot.encoding);
        let end = to_proto::position(&text, &index, span.end, snapshot.encoding);
        // Tokens are single-line constructs; clamp a stray wrap to its line.
        let length = if end.line == start.line {
            end.character.saturating_sub(start.character)
        } else {
            line_end_character(&text, &index, span.start, snapshot.encoding)
                .saturating_sub(start.character)
        };
        if length == 0 {
            continue;
        }
        let delta_line = start.line - prev_line;
        let delta_start = if delta_line == 0 {
            start.character - prev_start
        } else {
            start.character
        };
        data.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });
        (prev_line, prev_start) = (start.line, start.character);
    }

    Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
        result_id: None,
        data,
    })))
}

/// The character (in the negotiated encoding) of the line end at `offset`.
fn line_end_character(
    text: &str,
    index: &LineIndex<'_>,
    offset: usize,
    encoding: crate::PositionEncoding,
) -> u32 {
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |nl| offset + nl);
    to_proto::position(text, index, line_end, encoding).character
}

/// Tag-borne token spans in one top-level block: `#mine` tags, and the
/// named-group field sub-spans of custom tags (plan §10 T1). Callout bodies
/// are skipped (their spans are relative — parser convention).
fn tag_token_spans(block: &Block, out: &mut Vec<(Span, u32)>) {
    fn tag(tag: &Tag, out: &mut Vec<(Span, u32)>) {
        match &tag.kind {
            TagKind::Unknown { name, .. } if name == "mine" => {
                out.push((tag.span, TOKEN_MINE));
            }
            // Mismatched custom tags carry no fields, so they stay
            // uncoloured — the `tag::shape-mismatch` diagnostic marks them.
            TagKind::Custom { fields, .. } => {
                out.extend(fields.iter().map(|f| (f.span, TOKEN_CUSTOM_FIELD)));
            }
            _ => {}
        }
    }
    fn inline(content: &InlineContent, out: &mut Vec<(Span, u32)>) {
        for seg in &content.segments {
            match &seg.kind {
                InlineKind::Tag(t) => tag(t, out),
                InlineKind::Bold(inner)
                | InlineKind::Italic(inner)
                | InlineKind::Strikethrough(inner) => inline(inner, out),
                InlineKind::Link(link) => {
                    for t in &link.tags {
                        tag(t, out);
                    }
                }
                _ => {}
            }
        }
    }
    match block {
        Block::Paragraph(p) => inline(&p.content, out),
        Block::Heading(h) => inline(&h.content, out),
        Block::List(l) => {
            for item in &l.items {
                inline(&item.content, out);
                if let Some(desc) = &item.description {
                    inline(desc, out);
                }
                for child in &item.children {
                    tag_token_spans(child, out);
                }
            }
        }
        Block::BlockTag(t) => tag(t, out),
        Block::CodeBlock(cb) => {
            for t in &cb.tags {
                tag(t, out);
            }
        }
        Block::Table(t) => {
            for cell in t.headers.iter().chain(t.rows.iter().flatten()) {
                inline(cell, out);
            }
        }
        Block::FootnoteDefinition(f) => inline(&f.content, out),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{PartialResultParams, TextDocumentIdentifier, WorkDoneProgressParams};

    use super::*;

    /// Decode the delta encoding back to absolute `line:char len type`.
    fn check(files: &[(&str, &str)], expected: Expect) {
        check_with_tags(files, morg_parser::TagTable::empty(), expected);
    }

    fn check_with_tags(files: &[(&str, &str)], tags: morg_parser::TagTable, expected: Expect) {
        let (snap, position) =
            crate::handlers::test_support::snapshot_with_tags(files, &["wright2004"], tags);
        let response = handle(
            &snap,
            SemanticTokensParams {
                text_document: TextDocumentIdentifier {
                    uri: position.text_document.uri,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
        .unwrap();
        let Some(SemanticTokensResult::Tokens(tokens)) = response else {
            panic!("expected tokens, got {response:?}");
        };
        let names = [
            "topic",
            "argument",
            "source",
            "stance/op",
            "cite",
            "mine",
            "custom-field",
        ];
        let (mut line, mut start) = (0u32, 0u32);
        let rendered = tokens
            .data
            .iter()
            .map(|t| {
                line += t.delta_line;
                start = if t.delta_line == 0 { start } else { 0 } + t.delta_start;
                format!(
                    "{line}:{start} len {} {}",
                    t.length, names[t.token_type as usize]
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        expected.assert_eq(&rendered);
    }

    const SLA: &str =
        "---\nid: sla\nkind: topic\ntitle: \"SLA\"\n---\n\nEvidence [@wright2004]. #anchor e1\n";
    const SRC: &str = "---\nid: wright2004\nkind: source\n---\n";

    #[test]
    fn relations_stances_and_ops_are_highlighted() {
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/src.md", SRC),
                (
                    "/v/arg.md",
                    "---\nid: a$0rg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#e1, stance: supports, op: analyze }\n  - { to: gone, stance: opposes }\n  - { to: wright2004, stance: qualifies }\n---\n",
                ),
            ],
            expect![[r#"
                5:10 len 6 topic
                5:26 len 8 stance/op
                5:40 len 7 stance/op
                6:24 len 7 stance/op
                7:10 len 10 source
                7:30 len 9 stance/op"#]],
        );
    }

    #[test]
    fn body_links_cites_and_mine_are_highlighted() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/lmr.md",
                    "---\nid: l$0mr\nkind: topic\n---\n\nSee [sla](id:sla). Cited [@wright2004]. #mine\n",
                ),
            ],
            expect![[r#"
                5:4 len 13 topic
                5:25 len 13 cite
                5:40 len 5 mine"#]],
        );
    }

    #[test]
    fn custom_tag_fields_get_property_sub_spans() {
        let table = morg_parser::TagTable::build([morg_parser::TagDeclaration {
            name: "book".to_owned(),
            pattern: Some(r#""(?<title>[^"]+)"\s+by\s+(?<author>.+)"#.to_owned()),
            kind: None,
        }])
        .unwrap();
        check_with_tags(
            &[(
                "/v/read.md",
                "---\nid: r$0ead\nkind: topic\n---\n\nDone [@wright2004]. #book \"Dune\" by Frank Herbert\n\nBad one. #book untitled #mine\n",
            )],
            table,
            // Each matched named group gets its sub-span; the mismatched
            // occurrence stays uncoloured (its lint marks it) while `#mine`
            // on the same line still lands.
            expect![[r#"
                5:5 len 13 cite
                5:27 len 4 custom-field
                5:36 len 13 custom-field
                7:24 len 5 mine"#]],
        );
    }

    #[test]
    fn unresolved_targets_stay_uncoloured() {
        check(
            &[(
                "/v/a.md",
                "---\nid: a$0\nkind: topic\n---\n\nSee [x](id:nope).\n",
            )],
            expect![""],
        );
    }
}

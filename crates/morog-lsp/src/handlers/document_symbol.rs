//! `textDocument/documentSymbol` (plan §5.6): frontmatter, headings, and
//! evidence blocks labelled with their citekeys.

use lsp_types::{DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, Range, SymbolKind};
use morg_parser::{Block, LineIndex};

use crate::handlers::HandlerError;
use crate::state::Snapshot;
use crate::{from_proto, to_proto};

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: DocumentSymbolParams,
) -> Result<Option<DocumentSymbolResponse>, HandlerError> {
    let Some(file) = from_proto::abs_path(&params.text_document.uri)
        .and_then(|path| snapshot.analysis.file_id(&path))
    else {
        return Ok(None);
    };
    let Some(fa) = snapshot.analysis.file(file) else {
        return Ok(None);
    };
    let (Some(doc), Some(text)) = (&fa.doc, &fa.text) else {
        return Ok(None); // Facts-only entries have no outline to offer.
    };

    let index = LineIndex::new(text);
    let to_range = |span: morg_parser::Span| to_proto::range(text, &index, span, snapshot.encoding);

    let mut root: Vec<DocumentSymbol> = Vec::new();
    // Open heading sections, outermost first.
    let mut stack: Vec<(u8, DocumentSymbol)> = Vec::new();

    fn close_to(level: u8, stack: &mut Vec<(u8, DocumentSymbol)>, root: &mut Vec<DocumentSymbol>) {
        while stack.last().is_some_and(|(l, _)| *l >= level) {
            let (_, symbol) = stack.pop().expect("just checked");
            attach(symbol, stack, root);
        }
    }

    fn attach(
        symbol: DocumentSymbol,
        stack: &mut [(u8, DocumentSymbol)],
        root: &mut Vec<DocumentSymbol>,
    ) {
        match stack.last_mut() {
            Some((_, parent)) => {
                // A section covers everything nested under it.
                parent.range.end = parent.range.end.max(symbol.range.end);
                parent.children.get_or_insert_default().push(symbol);
            }
            None => root.push(symbol),
        }
    }

    if let Some(fm) = &doc.frontmatter {
        let note = &fa.facts.note;
        let name = match (&note.kind, &note.id) {
            (Some(kind), Some(id)) => format!("{} {id}", kind.as_str()),
            _ => "frontmatter".to_owned(),
        };
        root.push(symbol(
            name,
            note.title.clone(),
            SymbolKind::NAMESPACE,
            to_range(fm.span),
            note.id_span.map_or_else(|| to_range(fm.span), to_range),
        ));
    }

    // Facts blocks are the non-blank top-level blocks, in document order —
    // walk them alongside the parse tree to see heading levels.
    let mut facts_blocks = fa.facts.blocks.iter();
    for block in &doc.children {
        if matches!(block, Block::BlankLine(_)) {
            continue;
        }
        let block_facts = facts_blocks.next();
        if let Block::Heading(heading) = block {
            let level = heading.level;
            close_to(level, &mut stack, &mut root);
            let range = to_range(heading.span);
            stack.push((
                level,
                symbol(
                    heading.content.plain_text(),
                    None,
                    SymbolKind::STRING,
                    range,
                    range,
                ),
            ));
            continue;
        }
        // Evidence blocks, labelled with their citekeys (plan §5.6).
        if let Some(block_facts) = block_facts
            && block_facts.unit == morog_sema::BlockUnit::Evidence
        {
            let name = block_facts
                .cites
                .iter()
                .map(|cite| format!("@{}", cite.key.0))
                .collect::<Vec<_>>()
                .join(", ");
            let range = to_range(block_facts.span);
            let child = symbol(
                name,
                block_facts.anchor.as_ref().map(|a| format!("#{a}")),
                SymbolKind::KEY,
                range,
                range,
            );
            attach(child, &mut stack, &mut root);
            // Grow every open section over this block.
            for (_, open) in &mut stack {
                open.range.end = open.range.end.max(range.end);
            }
        }
    }
    close_to(0, &mut stack, &mut root);

    Ok(Some(DocumentSymbolResponse::Nested(root)))
}

fn symbol(
    name: String,
    detail: Option<String>,
    kind: SymbolKind,
    range: Range,
    selection_range: Range,
) -> DocumentSymbol {
    #[allow(deprecated)] // `deprecated` is a protocol field we must fill.
    DocumentSymbol {
        name,
        detail,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    }
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{PartialResultParams, TextDocumentIdentifier, WorkDoneProgressParams};

    use super::*;
    use crate::handlers::test_support::{fmt_position, snapshot};

    fn check(text: &str, expected: Expect) {
        let (snap, position) = snapshot(&[("/v/note.md", text)], &["wright2004", "reich1997"]);
        let response = handle(
            &snap,
            DocumentSymbolParams {
                text_document: TextDocumentIdentifier {
                    uri: position.text_document.uri,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
        .unwrap();
        let rendered = match response {
            Some(DocumentSymbolResponse::Nested(symbols)) => {
                let mut out = String::new();
                fn render(symbols: &[DocumentSymbol], depth: usize, out: &mut String) {
                    for s in symbols {
                        out.push_str(&format!(
                            "{}{:?} {}{} {}..{}\n",
                            "  ".repeat(depth),
                            s.kind,
                            s.name,
                            s.detail
                                .as_deref()
                                .map(|d| format!(" ({d})"))
                                .unwrap_or_default(),
                            fmt_position(s.range.start),
                            fmt_position(s.range.end),
                        ));
                        if let Some(children) = &s.children {
                            render(children, depth + 1, out);
                        }
                    }
                }
                render(&symbols, 0, &mut out);
                out
            }
            other => format!("{other:?}"),
        };
        expected.assert_eq(&rendered);
    }

    #[test]
    fn outline_has_frontmatter_headings_and_evidence() {
        check(
            "---\nid: s$0la\nkind: topic\ntitle: \"SLA\"\n---\n\n# Leaf economics\n\nSLA anchors the spectrum [@wright2004]. #anchor e1\n\n## Correlations\n\nPhotosynthesis correlates [@reich1997] [@wright2004]. #anchor e2\n\nMy own hunch. #mine\n\n# Loose end\n",
            expect![[r#"
                Namespace topic sla (SLA) 0:0..4:3
                String Leaf economics 6:0..12:64
                  Key @wright2004 (#e1) 8:0..8:50
                  String Correlations 10:0..12:64
                    Key @reich1997, @wright2004 (#e2) 12:0..12:64
                String Loose end 16:0..16:11
            "#]],
        );
    }

    #[test]
    fn plain_note_outline_is_headings_only() {
        check(
            "# On$0e\n\ntext\n\n## Two\n",
            expect![[r#"
                String One 0:0..4:6
                  String Two 4:0..4:6
            "#]],
        );
    }
}

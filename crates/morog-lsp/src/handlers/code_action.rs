//! `textDocument/codeAction` (plan §5.6):
//!
//! - **Create source note for @key** — a cite whose key has no source note
//!   yet; uses the same serialization path as `morog research source`
//!   (replicated in [`crate::research`]).
//! - **Mark block #mine** — the quick-fix for `sta::uncited-block`.
//! - **Add anchor to this block** — a stable address for a block; also, on
//!   a relation targeting a positional `id#bN` address, the quick-fix for
//!   `sta::positional-address` (anchor the addressed block — or reuse its
//!   existing anchor — and rewrite the relation).
//! - **Extract selection to new topic (carry citations)** — the selection
//!   moves into a fresh topic note, inline citations riding along, and a
//!   link to the new topic takes its place.
//! - **Add relation to frontmatter** — from an inline `id:` link to a topic
//!   in an argument's prose.

use std::collections::BTreeSet;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    CreateFile, Diagnostic, DocumentChangeOperation, NumberOrString, OneOf,
    OptionalVersionedTextDocumentIdentifier, Range, ResourceOp, TextDocumentEdit, TextEdit, Url,
};
use morg_parser::{LineIndex, Span};
use morog_sema::{BlockUnit, NoteKind};

use crate::analysis::FileAnalysis;
use crate::handlers::{Edits, HandlerError, file_text, first_words, id_pos_in_ref, workspace_edit};
use crate::state::Snapshot;
use crate::vfs::FileId;
use crate::{from_proto, research, to_proto};

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: CodeActionParams,
) -> Result<Option<CodeActionResponse>, HandlerError> {
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
    let end = from_proto::offset(&text, &index, params.range.end, snapshot.encoding).max(start);

    let cx = Cx {
        snapshot,
        file,
        fa: &fa,
        text: &text,
        params: &params,
        start,
        end,
        actions: Vec::new(),
    };
    Ok(Some(cx.collect()))
}

/// Everything the individual action builders share.
struct Cx<'a> {
    snapshot: &'a Snapshot,
    file: FileId,
    fa: &'a FileAnalysis,
    text: &'a str,
    params: &'a CodeActionParams,
    start: usize,
    end: usize,
    actions: Vec<CodeActionOrCommand>,
}

impl Cx<'_> {
    fn collect(mut self) -> CodeActionResponse {
        self.create_source_notes();
        self.mark_block_mine();
        self.add_anchor_to_block();
        self.fix_positional_addresses();
        self.extract_selection_to_topic();
        self.add_relation_to_frontmatter();
        self.actions
    }

    fn intersects(&self, span: Span) -> bool {
        span.start <= self.end && self.start <= span.end
    }

    /// The facts block containing the cursor (range start).
    fn block_at_cursor(&self) -> Option<&morog_sema::BlockFacts> {
        self.fa
            .facts
            .blocks
            .iter()
            .find(|block| block.span.start <= self.start && self.start <= block.span.end)
    }

    /// Context diagnostics with the given `sta::*` code — the client sends
    /// only the ones overlapping the requested range.
    fn matching_diagnostics(&self, code: &str) -> Vec<Diagnostic> {
        self.params
            .context
            .diagnostics
            .iter()
            .filter(|diag| diag.code == Some(NumberOrString::String(code.to_owned())))
            .cloned()
            .collect()
    }

    fn push(
        &mut self,
        title: String,
        kind: CodeActionKind,
        operations: Vec<DocumentChangeOperation>,
        diagnostics: Vec<Diagnostic>,
    ) {
        self.actions
            .push(CodeActionOrCommand::CodeAction(CodeAction {
                title,
                kind: Some(kind),
                diagnostics: (!diagnostics.is_empty()).then_some(diagnostics),
                edit: Some(workspace_edit(operations)),
                ..Default::default()
            }));
    }

    /// `CreateFile` + initial content for a new note in the vault root.
    fn create_note_operations(&self, uri: &Url, content: String) -> Vec<DocumentChangeOperation> {
        vec![
            DocumentChangeOperation::Op(ResourceOp::Create(CreateFile {
                uri: uri.clone(),
                options: None,
                annotation_id: None,
            })),
            DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: None,
                },
                edits: vec![OneOf::Left(TextEdit {
                    range: Range::default(),
                    new_text: content,
                })],
            }),
        ]
    }

    /// "Create source note for @key": offered on a cite whose key has no
    /// source note. With a bibliography loaded the key must exist in it
    /// (source ids must equal bibrat keys); with none, creation is
    /// unchecked, as in `morog research source`.
    fn create_source_notes(&mut self) {
        let mut offered: BTreeSet<&str> = BTreeSet::new();
        let cites = self
            .fa
            .facts
            .blocks
            .iter()
            .flat_map(|block| &block.cites)
            .filter(|cite| self.intersects(cite.span));
        let mut new: Vec<(String, Vec<DocumentChangeOperation>)> = Vec::new();
        for cite in cites {
            let key = cite.key.0.as_str();
            if !offered.insert(key) || self.snapshot.analysis.resolve_id(key).is_some() {
                continue;
            }
            let entry = self.snapshot.analysis.bib.get(key);
            if entry.is_none() && !self.snapshot.analysis.bib.is_empty() {
                continue; // unknown key: a source note would violate sta::id
            }
            let title = entry
                .and_then(|e| e.meta.as_ref())
                .and_then(|m| m.title.as_deref());
            let Some(uri) = to_proto::url(&self.snapshot.vault_root.join(format!("{key}.md")))
            else {
                continue;
            };
            let content = research::source_note(key, title);
            new.push((
                format!("Create source note for @{key}"),
                self.create_note_operations(&uri, content),
            ));
        }
        for (title, operations) in new {
            self.push(title, CodeActionKind::QUICKFIX, operations, Vec::new());
        }
    }

    /// "Mark block #mine": the quick-fix for `sta::uncited-block`.
    fn mark_block_mine(&mut self) {
        if self.fa.facts.note.kind != Some(NoteKind::Topic) {
            return;
        }
        let Some(block) = self.block_at_cursor() else {
            return;
        };
        if !block.citable || block.unit != BlockUnit::None {
            return;
        }
        let mut edits = Edits::default();
        edits.push(
            self.file,
            block.span.end,
            block.span.end,
            " #mine".to_owned(),
        );
        let diagnostics = self.matching_diagnostics("sta::uncited-block");
        let operations = edits.into_operations(self.snapshot);
        self.push(
            "Mark block #mine".to_owned(),
            CodeActionKind::QUICKFIX,
            operations,
            diagnostics,
        );
    }

    /// "Add anchor to this block": a stable address for the block under the
    /// cursor.
    fn add_anchor_to_block(&mut self) {
        if self.fa.facts.note.id.is_none() {
            return;
        }
        let Some(block) = self.block_at_cursor() else {
            return;
        };
        if !block.citable || block.anchor.is_some() {
            return;
        }
        let name = free_anchor(self.fa, block.idx);
        let mut edits = Edits::default();
        edits.push(
            self.file,
            block.span.end,
            block.span.end,
            format!(" #anchor {name}"),
        );
        let operations = edits.into_operations(self.snapshot);
        self.push(
            "Add anchor to this block".to_owned(),
            CodeActionKind::REFACTOR,
            operations,
            Vec::new(),
        );
    }

    /// The quick-fix for `sta::positional-address`: a relation targeting
    /// `id#bN` gets the addressed block anchored (or its existing anchor
    /// reused) and the relation rewritten to the stable name.
    fn fix_positional_addresses(&mut self) {
        let mut new = Vec::new();
        for rel in &self.fa.facts.relations {
            if !self.intersects(rel.span) {
                continue;
            }
            let Some(anchor) = rel.target_anchor.as_deref() else {
                continue;
            };
            let Some(n) = positional_index(anchor) else {
                continue;
            };
            let Some(target_file) = self.snapshot.analysis.resolve_id(&rel.target_id) else {
                continue;
            };
            let Some(target_fa) = self.snapshot.analysis.file(target_file).cloned() else {
                continue;
            };
            // `#bN` was morg's removed positional address: block index N.
            let Some(block) = target_fa.facts.blocks.iter().find(|b| b.idx == n) else {
                continue;
            };
            // The anchor half of this relation's `to:` text.
            let Some(slice) = self.text.get(rel.span.start..rel.span.end) else {
                continue;
            };
            let Some(id_rel) = id_pos_in_ref(slice, &rel.target_id) else {
                continue;
            };
            let anchor_start = rel.span.start + id_rel + rel.target_id.len() + 1;
            let anchor_end = anchor_start + anchor.len();
            if self.text.get(anchor_start..anchor_end) != Some(anchor) {
                continue;
            }

            let mut edits = Edits::default();
            let (title, name) = match &block.anchor {
                Some(existing) => (
                    format!("Replace positional address with `#{existing}`"),
                    existing.clone(),
                ),
                None => {
                    let name = free_anchor(&target_fa, block.idx);
                    edits.push(
                        target_file,
                        block.span.end,
                        block.span.end,
                        format!(" #anchor {name}"),
                    );
                    ("Add anchor to the addressed block".to_owned(), name)
                }
            };
            edits.push(self.file, anchor_start, anchor_end, name);
            new.push((title, edits.into_operations(self.snapshot)));
        }
        let diagnostics = self.matching_diagnostics("sta::positional-address");
        for (title, operations) in new {
            self.push(
                title,
                CodeActionKind::QUICKFIX,
                operations,
                diagnostics.clone(),
            );
        }
    }

    /// "Extract selection to new topic (carry citations)": the selected
    /// blocks move — citations included — into a fresh topic note, and a
    /// link to it takes their place.
    fn extract_selection_to_topic(&mut self) {
        if self.start >= self.end {
            return;
        }
        let body_start = self
            .fa
            .doc
            .as_ref()
            .and_then(|doc| doc.frontmatter.as_ref())
            .map_or(0, |fm| fm.span.end);
        if self.start < body_start {
            return;
        }
        let (Some(start), Some(end)) = (
            char_boundary(self.text, self.start),
            char_boundary(self.text, self.end),
        ) else {
            return;
        };
        let selection = self.text[start..end].trim();
        if selection.is_empty() {
            return;
        }
        let title = first_words(selection, 5)
            .trim_end_matches('…')
            .trim()
            .to_owned();
        let Some(id) = research::slugify(&title) else {
            return;
        };
        if self.snapshot.analysis.xref.by_id.contains_key(id.as_str()) {
            return;
        }
        let Some(uri) = to_proto::url(&self.snapshot.vault_root.join(format!("{id}.md"))) else {
            return;
        };
        let content = format!("{}\n{selection}\n", research::topic_note(&id, &title));
        let mut operations = self.create_note_operations(&uri, content);
        let mut edits = Edits::default();
        edits.push(self.file, start, end, format!("[{title}](id:{id})"));
        operations.extend(edits.into_operations(self.snapshot));
        self.push(
            "Extract selection to new topic (carry citations)".to_owned(),
            CodeActionKind::REFACTOR_EXTRACT,
            operations,
            Vec::new(),
        );
    }

    /// "Add relation to frontmatter": an inline `id:` link to a topic in an
    /// argument's prose becomes a typed relation.
    fn add_relation_to_frontmatter(&mut self) {
        if self.fa.facts.note.kind != Some(NoteKind::Argument) {
            return;
        }
        let Some(fm) = self
            .fa
            .doc
            .as_ref()
            .and_then(|doc| doc.frontmatter.as_ref())
        else {
            return;
        };
        let mut offered: BTreeSet<&str> = BTreeSet::new();
        let mut new = Vec::new();
        for id_ref in &self.fa.facts.id_refs {
            if !self.intersects(id_ref.span) || !offered.insert(&id_ref.target_id) {
                continue;
            }
            let target_kind = self
                .snapshot
                .analysis
                .resolve_id(&id_ref.target_id)
                .and_then(|file| self.snapshot.analysis.file(file))
                .and_then(|fa| fa.facts.note.kind);
            if target_kind != Some(NoteKind::Topic) {
                continue;
            }
            if self
                .fa
                .facts
                .relations
                .iter()
                .any(|rel| rel.target_id == id_ref.target_id)
            {
                continue;
            }
            let id = &id_ref.target_id;
            let entry = format!("  - {{ to: {id}, stance: supports }}");
            let (offset, insert) = match fm.entry("relations") {
                // Append to the existing list.
                Some(existing) => (existing.value_span.end, format!("\n{entry}")),
                // Start the list just before the closing delimiter.
                None if self.text.get(fm.span.end - 3..fm.span.end) == Some("---") => {
                    (fm.span.end - 3, format!("relations:\n{entry}\n"))
                }
                None => continue,
            };
            let mut edits = Edits::default();
            edits.push(self.file, offset, offset, insert);
            new.push((
                format!("Add relation to `{id}` in frontmatter"),
                edits.into_operations(self.snapshot),
            ));
        }
        for (title, operations) in new {
            self.push(title, CodeActionKind::REFACTOR, operations, Vec::new());
        }
    }
}

/// An anchor name free in `fa`, based on the evidence-block convention of
/// `morog research topic` (`evidence-N`, 1-based block index).
fn free_anchor(fa: &FileAnalysis, idx: u32) -> String {
    let taken = |name: &str| {
        fa.facts
            .blocks
            .iter()
            .any(|block| block.anchor.as_deref() == Some(name))
    };
    let base = format!("evidence-{}", idx + 1);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .expect("some suffix is free")
}

/// Positional `#bN` → N (plan §2.4's removed address shape).
fn positional_index(anchor: &str) -> Option<u32> {
    anchor.strip_prefix('b')?.parse().ok()
}

/// Snap an offset down to a char boundary.
fn char_boundary(text: &str, mut offset: usize) -> Option<usize> {
    offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{
        CodeActionContext, PartialResultParams, Position, TextDocumentIdentifier,
        WorkDoneProgressParams,
    };

    use super::*;
    use crate::handlers::test_support::{fmt_position, snapshot};
    use crate::state::Snapshot;

    /// Render actions as `title [kind] (diags)` plus their operations.
    fn render(actions: CodeActionResponse) -> String {
        if actions.is_empty() {
            return "<empty>".to_owned();
        }
        let mut out = Vec::new();
        for action in actions {
            let CodeActionOrCommand::CodeAction(action) = action else {
                panic!("commands are not produced")
            };
            let diags = action
                .diagnostics
                .as_ref()
                .map(|diags| format!(" ({} diagnostic(s))", diags.len()))
                .unwrap_or_default();
            out.push(format!(
                "{} [{}]{diags}",
                action.title,
                action.kind.as_ref().unwrap().as_str()
            ));
            let Some(lsp_types::DocumentChanges::Operations(operations)) =
                action.edit.and_then(|edit| edit.document_changes)
            else {
                panic!("documentChanges expected")
            };
            for op in operations {
                match op {
                    DocumentChangeOperation::Op(ResourceOp::Create(create)) => {
                        out.push(format!("  create {}", create.uri.path()));
                    }
                    DocumentChangeOperation::Edit(edit) => {
                        for item in &edit.edits {
                            let OneOf::Left(text_edit) = item else {
                                panic!()
                            };
                            out.push(format!(
                                "  edit   {} {}..{} -> {:?}",
                                edit.text_document.uri.path(),
                                fmt_position(text_edit.range.start),
                                fmt_position(text_edit.range.end),
                                sanitize(&text_edit.new_text),
                            ));
                        }
                    }
                    other => out.push(format!("  {other:?}")),
                }
            }
        }
        out.join("\n")
    }

    /// Stabilize the generated identity fields for snapshotting.
    fn sanitize(new_text: &str) -> String {
        new_text
            .split_inclusive('\n')
            .map(|line| match line {
                line if line.starts_with("uuid: ") => "uuid: <uuid>\n",
                line if line.starts_with("created: ") => "created: <date>\n",
                line => line,
            })
            .collect()
    }

    /// Run code actions over the `$0`-marked position, selecting
    /// `selection` characters from it, with `diag_codes` in the context.
    fn run(
        snap: &Snapshot,
        position: lsp_types::TextDocumentPositionParams,
        selection: &str,
        diag_codes: &[&str],
    ) -> String {
        let end = end_position(position.position, selection);
        let diagnostics = diag_codes
            .iter()
            .map(|code| Diagnostic {
                range: Range::new(position.position, end),
                code: Some(NumberOrString::String((*code).to_owned())),
                message: "from the publish set".to_owned(),
                ..Default::default()
            })
            .collect();
        let response = handle(
            snap,
            CodeActionParams {
                text_document: TextDocumentIdentifier {
                    uri: position.text_document.uri,
                },
                range: Range::new(position.position, end),
                context: CodeActionContext {
                    diagnostics,
                    only: None,
                    trigger_kind: None,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
        .unwrap();
        render(response.unwrap())
    }

    /// The position `selection` characters past `start` (single- or
    /// multi-line ASCII selections).
    fn end_position(start: Position, selection: &str) -> Position {
        let lines = selection.matches('\n').count() as u32;
        if lines == 0 {
            Position::new(start.line, start.character + selection.len() as u32)
        } else {
            let last = selection.rsplit('\n').next().unwrap_or_default();
            Position::new(start.line + lines, last.len() as u32)
        }
    }

    fn check(files: &[(&str, &str)], selection: &str, diag_codes: &[&str], expected: Expect) {
        let (snap, position) = snapshot(files, &["wright2004"]);
        expected.assert_eq(&run(&snap, position, selection, diag_codes));
    }

    const SLA: &str = "---\nid: sla\nkind: topic\ntitle: \"SLA\"\n---\n\nEvidence [@wright2004]. #anchor e1\n\nBare block without provenance.\n";

    #[test]
    fn create_source_note_for_uncreated_citekey() {
        check(
            &[(
                "/v/sla.md",
                SLA.replacen("[@wright2004]", "[@wright$02004]", 1).leak(),
            )],
            "",
            &[],
            expect![[r#"
                Create source note for @wright2004 [quickfix]
                  create /v/wright2004.md
                  edit   /v/wright2004.md 0:0..0:0 -> "---\nid: wright2004\nuuid: <uuid>\nkind: source\ntitle: \"The wright2004 paper\"\nstatus: unread\ncreated: <date>\n---\n""#]],
        );
    }

    #[test]
    fn mark_mine_is_the_uncited_block_quickfix() {
        check(
            &[(
                "/v/sla.md",
                SLA.replacen("Bare block", "Bare$0 block", 1).leak(),
            )],
            "",
            &["sta::uncited-block"],
            expect![[r#"
                Mark block #mine [quickfix] (1 diagnostic(s))
                  edit   /v/sla.md 8:30..8:30 -> " #mine"
                Add anchor to this block [refactor]
                  edit   /v/sla.md 8:30..8:30 -> " #anchor evidence-2""#]],
        );
    }

    #[test]
    fn positional_address_quickfix_reuses_or_creates_the_anchor() {
        // The addressed block (index 0) already has #e1: reuse it.
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#b$00, stance: supports }\n---\n",
                ),
            ],
            "",
            &["sta::positional-address"],
            expect![[r#"
                Replace positional address with `#e1` [quickfix] (1 diagnostic(s))
                  edit   /v/arg.md 5:14..5:16 -> "e1""#]],
        );
        // The addressed block (index 1) has none: anchor it and rewrite.
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#b$01, stance: supports }\n---\n",
                ),
            ],
            "",
            &["sta::positional-address"],
            expect![[r#"
                Add anchor to the addressed block [quickfix] (1 diagnostic(s))
                  edit   /v/sla.md 8:30..8:30 -> " #anchor evidence-2"
                  edit   /v/arg.md 5:14..5:16 -> "evidence-2""#]],
        );
    }

    #[test]
    fn extract_selection_to_new_topic() {
        check(
            &[(
                "/v/sla.md",
                SLA.replacen("Bare block", "Bare$0 block", 1).leak(),
            )],
            "block without provenance.",
            &[],
            expect![[r#"
                Mark block #mine [quickfix]
                  edit   /v/sla.md 8:30..8:30 -> " #mine"
                Add anchor to this block [refactor]
                  edit   /v/sla.md 8:30..8:30 -> " #anchor evidence-2"
                Extract selection to new topic (carry citations) [refactor.extract]
                  create /v/block-without-provenance.md
                  edit   /v/block-without-provenance.md 0:0..0:0 -> "---\nid: block-without-provenance\nuuid: <uuid>\nkind: topic\ntitle: \"block without provenance\"\ncreated: <date>\n---\n\nblock without provenance\n"
                  edit   /v/sla.md 8:4..8:29 -> "[block without provenance](id:block-without-provenance)""#]],
        );
    }

    #[test]
    fn add_relation_from_inline_link_in_argument_prose() {
        // With an existing relations list, the entry is appended to it.
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/lmr.md", "---\nid: lmr\nkind: topic\n---\n"),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: supports }\n---\n\nSee [lmr](id:l$0mr). #mine\n",
                ),
            ],
            "",
            &[],
            expect![[r#"
                Add anchor to this block [refactor]
                  edit   /v/arg.md 8:24..8:24 -> " #anchor evidence-1"
                Add relation to `lmr` in frontmatter [refactor]
                  edit   /v/arg.md 5:33..5:33 -> "\n  - { to: lmr, stance: supports }""#]],
        );
        // Without one, `relations:` is started before the closing ---.
        check(
            &[
                ("/v/lmr.md", "---\nid: lmr\nkind: topic\n---\n"),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: argument\nthesis: \"t\"\n---\n\nSee [lmr](id:l$0mr). #mine\n",
                ),
            ],
            "",
            &[],
            expect![[r#"
                Add anchor to this block [refactor]
                  edit   /v/arg.md 6:24..6:24 -> " #anchor evidence-1"
                Add relation to `lmr` in frontmatter [refactor]
                  edit   /v/arg.md 4:0..4:0 -> "relations:\n  - { to: lmr, stance: supports }\n""#]],
        );
        // A link already covered by a relation offers nothing.
        check(
            &[
                ("/v/lmr.md", "---\nid: lmr\nkind: topic\n---\n"),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: lmr, stance: supports }\n---\n\nSee [lmr](id:l$0mr). #mine\n",
                ),
            ],
            "",
            &[],
            expect![[r#"
                Add anchor to this block [refactor]
                  edit   /v/arg.md 8:24..8:24 -> " #anchor evidence-1""#]],
        );
    }
}

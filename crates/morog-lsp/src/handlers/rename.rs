//! `textDocument/prepareRename` + `rename` (plan §5.6).
//!
//! Renaming an **id** edits the `id:` declaration and every reference across
//! the vault — `to:` values (including the id prefix of `id#anchor`
//! targets), `related:` entries, and inline `id:` links — plus a
//! `RenameFile` when the filename equals `<id>.md`. The `uuid` field is the
//! permanent identity and never changes. Renaming a **source** id is refused
//! (it must equal the bibrat key; plan §4.2). Renaming an **anchor**
//! rewrites the `#anchor name` declaration and every `id#anchor` reference.
//!
//! Citation keys (`[@key]`) are owned by the bibliography and refused too —
//! and since source ids can't change, cite references never need rewriting.

use std::collections::BTreeMap;

use lsp_server::ErrorCode;
use lsp_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, OptionalVersionedTextDocumentIdentifier,
    PrepareRenameResponse, RenameFile, RenameParams, ResourceOp, TextDocumentEdit,
    TextDocumentPositionParams, TextEdit, WorkspaceEdit,
};
use morg_parser::LineIndex;
use morog_sema::{NoteKind, RelationKind, is_slug};

use crate::analysis::cursor::{self, CursorContext};
use crate::handlers::{FilePosition, HandlerError, file_position, file_text, file_uri};
use crate::state::Snapshot;
use crate::to_proto;
use crate::vfs::FileId;

/// What a rename at some position would do.
enum RenameTarget {
    /// Rename a note id vault-wide; the range is the id text under the
    /// cursor.
    Id { old: String, range: (usize, usize) },
    /// Rename a block anchor of `note_id`; the range is the anchor text
    /// under the cursor.
    Anchor {
        note_id: String,
        old: String,
        range: (usize, usize),
    },
}

pub(crate) fn prepare(
    snapshot: &Snapshot,
    params: TextDocumentPositionParams,
) -> Result<Option<PrepareRenameResponse>, HandlerError> {
    let Some(pos) = file_position(snapshot, &params) else {
        return Ok(None);
    };
    let target = classify_rename(snapshot, &pos)?;
    let index = LineIndex::new(&pos.text);
    let (placeholder, (start, end)) = match &target {
        RenameTarget::Id { old, range } => (old.clone(), *range),
        RenameTarget::Anchor { old, range, .. } => (old.clone(), *range),
    };
    Ok(Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: to_proto::range(
            &pos.text,
            &index,
            morg_parser::Span::new(start, end, 1, 1),
            snapshot.encoding,
        ),
        placeholder,
    }))
}

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: RenameParams,
) -> Result<Option<WorkspaceEdit>, HandlerError> {
    let Some(pos) = file_position(snapshot, &params.text_document_position) else {
        return Ok(None);
    };
    let new = params.new_name.as_str();
    match classify_rename(snapshot, &pos)? {
        RenameTarget::Id { old, .. } => rename_id(snapshot, &old, new).map(Some),
        RenameTarget::Anchor { note_id, old, .. } => {
            rename_anchor(snapshot, &note_id, &old, new).map(Some)
        }
    }
}

fn refuse<T>(message: impl Into<String>) -> Result<T, HandlerError> {
    Err((ErrorCode::InvalidRequest, message.into()))
}

/// Classify the cursor position into a rename target, applying the
/// refusals.
fn classify_rename(snapshot: &Snapshot, pos: &FilePosition) -> Result<RenameTarget, HandlerError> {
    match cursor::classify(&pos.fa, pos.offset) {
        CursorContext::IdDecl { id, span } => {
            refuse_source(snapshot, &id)?;
            Ok(RenameTarget::Id {
                old: id,
                range: (span.start, span.end),
            })
        }
        CursorContext::LinkTarget {
            target_id,
            anchor,
            span,
        } => {
            let slice = pos
                .text
                .get(span.start..span.end)
                .ok_or((ErrorCode::InternalError, "stale span".to_owned()))?;
            let id_rel = id_pos_in_ref(slice, &target_id)
                .ok_or((ErrorCode::InternalError, "unlocatable reference".to_owned()))?;
            let id_range = (span.start + id_rel, span.start + id_rel + target_id.len());
            if let Some(anchor) = anchor {
                let anchor_start = id_range.1 + 1; // past the `#`
                let anchor_range = (anchor_start, anchor_start + anchor.len());
                if pos.offset >= anchor_range.0
                    && pos.offset <= anchor_range.1
                    && pos.text.get(anchor_range.0..anchor_range.1) == Some(anchor.as_str())
                {
                    return Ok(RenameTarget::Anchor {
                        note_id: target_id,
                        old: anchor,
                        range: anchor_range,
                    });
                }
            }
            refuse_source(snapshot, &target_id)?;
            Ok(RenameTarget::Id {
                old: target_id,
                range: id_range,
            })
        }
        CursorContext::AnchorDecl { name, span } => {
            let note_id = pos
                .fa
                .facts
                .note
                .id
                .as_ref()
                .map(|id| id.as_str().to_owned())
                .ok_or((
                    ErrorCode::InvalidRequest,
                    "this note declares no id".to_owned(),
                ))?;
            // The tag span ends exactly at the anchor name.
            Ok(RenameTarget::Anchor {
                note_id,
                old: name.clone(),
                range: (span.end - name.len(), span.end),
            })
        }
        CursorContext::Cite { .. } => {
            refuse("citation keys are owned by the bibliography; rename the bibrat entry instead")
        }
        _ => refuse("nothing renameable here"),
    }
}

/// Renaming a source id is refused: it must equal the bibrat key
/// (plan §5.6).
fn refuse_source(snapshot: &Snapshot, id: &str) -> Result<(), HandlerError> {
    let kind = snapshot
        .analysis
        .resolve_id(id)
        .and_then(|file| snapshot.analysis.file(file))
        .and_then(|fa| fa.facts.note.kind);
    if kind == Some(NoteKind::Source) {
        return refuse(format!(
            "`{id}` is a source note; its id must equal the bibrat key and cannot be renamed"
        ));
    }
    Ok(())
}

/// Where a reference slice mentions its target id: after `(id:` for inline
/// links, at the (possibly quoted) start for frontmatter `to:` values.
fn id_pos_in_ref(slice: &str, target_id: &str) -> Option<usize> {
    if let Some(p) = slice.find("(id:") {
        let pos = p + 4;
        return slice[pos..].starts_with(target_id).then_some(pos);
    }
    let pos = slice.len() - slice.trim_start_matches(['"', '\'']).len();
    slice[pos..].starts_with(target_id).then_some(pos)
}

/// Byte-range edits per file, turned into a `WorkspaceEdit` at the end.
#[derive(Default)]
struct Edits {
    by_file: BTreeMap<FileId, Vec<(usize, usize, String)>>,
}

impl Edits {
    fn push(&mut self, file: FileId, start: usize, end: usize, new_text: String) {
        self.by_file
            .entry(file)
            .or_default()
            .push((start, end, new_text));
    }

    /// Convert to `documentChanges`, appending `extra` operations (file
    /// renames) after the text edits.
    fn into_workspace_edit(
        self,
        snapshot: &Snapshot,
        extra: Vec<DocumentChangeOperation>,
    ) -> Result<WorkspaceEdit, HandlerError> {
        let mut operations = Vec::with_capacity(self.by_file.len() + extra.len());
        for (file, mut edits) in self.by_file {
            let (Some(uri), Some(text)) = (file_uri(snapshot, file), file_text(snapshot, file))
            else {
                continue;
            };
            let index = LineIndex::new(&text);
            edits.sort_by_key(|&(start, ..)| start);
            edits.dedup();
            let edits = edits
                .into_iter()
                .map(|(start, end, new_text)| {
                    OneOf::Left(TextEdit {
                        range: to_proto::range(
                            &text,
                            &index,
                            morg_parser::Span::new(start, end, 1, 1),
                            snapshot.encoding,
                        ),
                        new_text,
                    })
                })
                .collect();
            operations.push(DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
                edits,
            }));
        }
        operations.extend(extra);
        Ok(WorkspaceEdit {
            changes: None,
            document_changes: Some(DocumentChanges::Operations(operations)),
            change_annotations: None,
        })
    }
}

/// Rename a note id across the vault.
fn rename_id(snapshot: &Snapshot, old: &str, new: &str) -> Result<WorkspaceEdit, HandlerError> {
    if !is_slug(new) {
        return refuse(format!("`{new}` is not a slug-shaped id"));
    }
    if new != old && snapshot.analysis.xref.by_id.contains_key(new) {
        return refuse(format!("id `{new}` is already declared in the vault"));
    }

    let mut edits = Edits::default();

    // The declaration (`uuid` stays untouched — it is the permanent
    // identity).
    let decl_file = snapshot.analysis.resolve_id(old);
    if let Some(file) = decl_file
        && let Some(span) = snapshot
            .analysis
            .file(file)
            .and_then(|fa| fa.facts.note.id_span)
    {
        edits.push(file, span.start, span.end, new.to_owned());
    }

    // Every reference. Cites never appear: they reference source ids, and
    // source renames are refused before we get here.
    for r in snapshot.analysis.xref.refs_to(old) {
        if matches!(r.kind, RelationKind::Cites) {
            continue;
        }
        let Some(text) = file_text(snapshot, r.file) else {
            continue;
        };
        let Some(slice) = text.get(r.span.start..r.span.end) else {
            continue;
        };
        if let Some(rel) = id_pos_in_ref(slice, old) {
            let start = r.span.start + rel;
            edits.push(r.file, start, start + old.len(), new.to_owned());
        }
    }

    // RenameFile when the filename follows the id (plan §5.6).
    let mut operations = Vec::new();
    if let Some(file) = decl_file
        && let Some(path) = snapshot.analysis.path(file)
        && path.file_stem().and_then(|stem| stem.to_str()) == Some(old)
        && let Some(old_uri) = file_uri(snapshot, file)
        && let Some(new_uri) = to_proto::url(&path.with_file_name(format!("{new}.md")))
    {
        operations.push(DocumentChangeOperation::Op(ResourceOp::Rename(
            RenameFile {
                old_uri,
                new_uri,
                options: None,
                annotation_id: None,
            },
        )));
    }

    edits.into_workspace_edit(snapshot, operations)
}

/// Rename a block anchor of `note_id`: the `#anchor name` declaration and
/// every `id#anchor` reference.
fn rename_anchor(
    snapshot: &Snapshot,
    note_id: &str,
    old: &str,
    new: &str,
) -> Result<WorkspaceEdit, HandlerError> {
    if !is_slug(new) {
        return refuse(format!("`{new}` is not a slug-shaped anchor name"));
    }
    let Some(decl_file) = snapshot.analysis.resolve_id(note_id) else {
        return refuse(format!("`{note_id}` does not resolve to any note"));
    };
    let fa = snapshot
        .analysis
        .file(decl_file)
        .cloned()
        .ok_or((ErrorCode::InternalError, "missing analysis".to_owned()))?;
    if new != old
        && fa
            .facts
            .blocks
            .iter()
            .any(|block| block.anchor.as_deref() == Some(new))
    {
        return refuse(format!("`{note_id}` already declares an anchor `#{new}`"));
    }

    let mut edits = Edits::default();

    // The `#anchor name` declaration, located textually within the
    // declaring block (works for cold, facts-only files too).
    let Some(decl_text) = file_text(snapshot, decl_file) else {
        return refuse(format!("cannot read the note declaring `{note_id}`"));
    };
    let decl_block = fa
        .facts
        .blocks
        .iter()
        .find(|block| block.anchor.as_deref() == Some(old));
    let Some((start, end)) = decl_block.and_then(|block| {
        let slice = decl_text.get(block.span.start..block.span.end)?;
        let (rel_start, rel_end) = anchor_name_range(slice, old)?;
        Some((block.span.start + rel_start, block.span.start + rel_end))
    }) else {
        return refuse(format!("`{note_id}` declares no anchor `#{old}`"));
    };
    edits.push(decl_file, start, end, new.to_owned());

    // Every `id#anchor` reference.
    for r in snapshot.analysis.xref.refs_to(note_id) {
        if r.target_anchor.as_deref() != Some(old) {
            continue;
        }
        let Some(text) = file_text(snapshot, r.file) else {
            continue;
        };
        let Some(slice) = text.get(r.span.start..r.span.end) else {
            continue;
        };
        if let Some(rel) = id_pos_in_ref(slice, note_id) {
            let anchor_start = r.span.start + rel + note_id.len() + 1;
            if text.get(anchor_start..anchor_start + old.len()) == Some(old) {
                edits.push(
                    r.file,
                    anchor_start,
                    anchor_start + old.len(),
                    new.to_owned(),
                );
            }
        }
    }

    edits.into_workspace_edit(snapshot, Vec::new())
}

/// The byte range of `old` inside a block slice's `#anchor old` tag.
fn anchor_name_range(slice: &str, old: &str) -> Option<(usize, usize)> {
    let mut from = 0;
    while let Some(found) = slice[from..].find("#anchor") {
        let abs = from + found;
        let rest = &slice[abs + "#anchor".len()..];
        let ws = rest.len() - rest.trim_start().len();
        if ws > 0 && rest[ws..].starts_with(old) {
            let after = rest[ws + old.len()..].chars().next();
            if after.is_none_or(|c| !(c.is_alphanumeric() || c == '-' || c == '_')) {
                let start = abs + "#anchor".len() + ws;
                return Some((start, start + old.len()));
            }
        }
        from = abs + "#anchor".len();
    }
    None
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::WorkDoneProgressParams;

    use super::*;
    use crate::handlers::test_support::{fmt_position, snapshot};

    fn render_edit(edit: &WorkspaceEdit) -> String {
        let Some(DocumentChanges::Operations(operations)) = &edit.document_changes else {
            return format!("{edit:?}");
        };
        let mut out = Vec::new();
        for op in operations {
            match op {
                DocumentChangeOperation::Edit(doc_edit) => {
                    for item in &doc_edit.edits {
                        let OneOf::Left(text_edit) = item else {
                            panic!("no annotated edits expected")
                        };
                        out.push(format!(
                            "edit   {} {}..{} -> `{}`",
                            doc_edit.text_document.uri.path(),
                            fmt_position(text_edit.range.start),
                            fmt_position(text_edit.range.end),
                            text_edit.new_text,
                        ));
                    }
                }
                DocumentChangeOperation::Op(ResourceOp::Rename(rename)) => out.push(format!(
                    "rename {} -> {}",
                    rename.old_uri.path(),
                    rename.new_uri.path()
                )),
                other => out.push(format!("{other:?}")),
            }
        }
        out.join("\n")
    }

    fn check_rename(files: &[(&str, &str)], new_name: &str, expected: Expect) {
        let (snap, position) = snapshot(files, &["wright2004"]);
        let rendered = match handle(
            &snap,
            RenameParams {
                text_document_position: position,
                new_name: new_name.to_owned(),
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        ) {
            Ok(Some(edit)) => render_edit(&edit),
            Ok(None) => "<none>".to_owned(),
            Err((_, message)) => format!("refused: {message}"),
        };
        expected.assert_eq(&rendered);
    }

    fn check_prepare(files: &[(&str, &str)], expected: Expect) {
        let (snap, position) = snapshot(files, &["wright2004"]);
        let rendered = match prepare(&snap, position) {
            Ok(Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder })) => {
                format!(
                    "{}..{} `{placeholder}`",
                    fmt_position(range.start),
                    fmt_position(range.end)
                )
            }
            Ok(other) => format!("{other:?}"),
            Err((_, message)) => format!("refused: {message}"),
        };
        expected.assert_eq(&rendered);
    }

    const SLA: &str = "---\nid: sla\nuuid: 0192f3c4-7a11-7bbb-8f55-111111111111\nkind: topic\ntitle: \"SLA\"\n---\n\nEvidence [@wright2004]. #anchor e1\n";
    const LMR: &str =
        "---\nid: lmr\nkind: topic\nrelated: [sla]\n---\n\nSee [sla](id:sla#e1). #mine\n";
    const ARG: &str = "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#e1, stance: supports }\n  - { to: sla, stance: qualifies }\n---\n";
    const SRC: &str = "---\nid: wright2004\nkind: source\n---\n";

    #[test]
    fn prepare_on_id_decl_and_reference() {
        check_prepare(
            &[("/v/sla.md", SLA.replacen("id: sla", "id: s$0la", 1).leak())],
            expect!["1:4..1:7 `sla`"],
        );
        check_prepare(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/arg.md",
                    ARG.replacen("to: sla,", "to: s$0la,", 1).leak(),
                ),
            ],
            expect!["6:10..6:13 `sla`"],
        );
        // On the anchor half of a target, the anchor is what renames.
        check_prepare(
            &[
                ("/v/sla.md", SLA),
                ("/v/arg.md", ARG.replacen("sla#e1", "sla#e$01", 1).leak()),
            ],
            expect!["5:14..5:16 `e1`"],
        );
    }

    #[test]
    fn rename_id_edits_declaration_references_and_file() {
        check_rename(
            &[
                ("/v/sla.md", SLA.replacen("id: sla", "id: s$0la", 1).leak()),
                ("/v/lmr.md", LMR),
                ("/v/arg.md", ARG),
            ],
            "specific-leaf-area",
            expect![[r#"
                edit   /v/sla.md 1:4..1:7 -> `specific-leaf-area`
                edit   /v/lmr.md 3:10..3:13 -> `specific-leaf-area`
                edit   /v/lmr.md 6:13..6:16 -> `specific-leaf-area`
                edit   /v/arg.md 5:10..5:13 -> `specific-leaf-area`
                edit   /v/arg.md 6:10..6:13 -> `specific-leaf-area`
                rename /v/sla.md -> /v/specific-leaf-area.md"#]],
        );
    }

    #[test]
    fn rename_id_refusals() {
        // A source id must equal the bibrat key.
        check_rename(
            &[(
                "/v/src.md",
                SRC.replacen("id: wright2004", "id: wright$02004", 1).leak(),
            )],
            "other",
            expect![
                "refused: `wright2004` is a source note; its id must equal the bibrat key and cannot be renamed"
            ],
        );
        // The new id must be a slug.
        check_rename(
            &[("/v/sla.md", SLA.replacen("id: sla", "id: s$0la", 1).leak())],
            "not a slug",
            expect!["refused: `not a slug` is not a slug-shaped id"],
        );
        // The new id must be free.
        check_rename(
            &[
                ("/v/sla.md", SLA.replacen("id: sla", "id: s$0la", 1).leak()),
                ("/v/lmr.md", LMR),
            ],
            "lmr",
            expect!["refused: id `lmr` is already declared in the vault"],
        );
        // Citation keys belong to the bibliography.
        check_rename(
            &[(
                "/v/sla.md",
                SLA.replacen("[@wright2004]", "[@wright$02004]", 1).leak(),
            )],
            "other",
            expect![
                "refused: citation keys are owned by the bibliography; rename the bibrat entry instead"
            ],
        );
    }

    #[test]
    fn rename_anchor_rewrites_declaration_and_references() {
        // From the declaration site.
        check_rename(
            &[
                (
                    "/v/sla.md",
                    SLA.replacen("#anchor e1", "#anchor e$01", 1).leak(),
                ),
                ("/v/lmr.md", LMR),
                ("/v/arg.md", ARG),
            ],
            "evidence-1",
            expect![[r#"
                edit   /v/sla.md 7:32..7:34 -> `evidence-1`
                edit   /v/lmr.md 6:17..6:19 -> `evidence-1`
                edit   /v/arg.md 5:14..5:16 -> `evidence-1`"#]],
        );
        // From a reference site.
        check_rename(
            &[
                ("/v/sla.md", SLA),
                ("/v/lmr.md", LMR),
                ("/v/arg.md", ARG.replacen("sla#e1", "sla#e$01", 1).leak()),
            ],
            "evidence-1",
            expect![[r#"
                edit   /v/sla.md 7:32..7:34 -> `evidence-1`
                edit   /v/lmr.md 6:17..6:19 -> `evidence-1`
                edit   /v/arg.md 5:14..5:16 -> `evidence-1`"#]],
        );
    }

    #[test]
    fn rename_anchor_refuses_collisions() {
        check_rename(
            &[(
                "/v/sla.md",
                "---\nid: sla\nkind: topic\n---\n\nA [@wright2004]. #anchor e$01\n\nB [@wright2004]. #anchor e2\n",
            )],
            "e2",
            expect!["refused: `sla` already declares an anchor `#e2`"],
        );
    }
}

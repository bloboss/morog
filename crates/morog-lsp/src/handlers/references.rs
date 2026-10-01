//! `textDocument/references` (plan §5.6): backlinks from the xref maps,
//! ordered by relation kind, stance, and op (the grouping clients can't
//! label is at least stable).

use lsp_types::{Location, ReferenceParams};
use morog_sema::RelationKind;

use crate::analysis::cursor::{self, CursorContext};
use crate::analysis::xref::RelationRef;
use crate::handlers::{HandlerError, file_position, location};
use crate::state::Snapshot;

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: ReferenceParams,
) -> Result<Option<Vec<Location>>, HandlerError> {
    let Some(pos) = file_position(snapshot, &params.text_document_position) else {
        return Ok(None);
    };
    let target = match cursor::classify(&pos.fa, pos.offset) {
        CursorContext::LinkTarget { target_id, .. } => target_id,
        CursorContext::IdDecl { id, .. } => id,
        CursorContext::Cite { key, .. } => key,
        _ => return Ok(None),
    };

    let mut refs: Vec<&RelationRef> = snapshot.analysis.xref.refs_to(&target).collect();
    // Stable grouping: argues (by stance, then op) first, then related,
    // mentions, cites; ties by file.
    refs.sort_by_key(|r| (rank(&r.kind), r.file));

    let mut locations = Vec::with_capacity(refs.len() + 1);
    if params.context.include_declaration
        && let Some(file) = snapshot.analysis.resolve_id(&target)
        && let Some(span) = snapshot
            .analysis
            .file(file)
            .and_then(|fa| fa.facts.note.id_span)
        && let Some(decl) = location(snapshot, file, span)
    {
        locations.push(decl);
    }
    locations.extend(
        refs.iter()
            .filter_map(|r| location(snapshot, r.file, r.span)),
    );
    Ok(Some(locations))
}

/// Ordering key: stance and op spread argues references apart (plan §5.6 —
/// "grouped by stance and op").
fn rank(kind: &RelationKind) -> (u8, u8, u8) {
    match kind {
        RelationKind::Argues { stance, op } => {
            (0, *stance as u8, op.map_or(u8::MAX, |o| o.ordinal()))
        }
        RelationKind::Related => (1, 0, 0),
        RelationKind::Mentions => (2, 0, 0),
        RelationKind::Cites => (3, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{PartialResultParams, ReferenceContext, WorkDoneProgressParams};

    use super::*;
    use crate::handlers::test_support::{fmt_location, snapshot};

    fn check(files: &[(&str, &str)], include_declaration: bool, expected: Expect) {
        let (snap, position) = snapshot(files, &["wright2004"]);
        let response = handle(
            &snap,
            ReferenceParams {
                text_document_position: position,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                context: ReferenceContext {
                    include_declaration,
                },
            },
        )
        .unwrap();
        let rendered = response.map_or("<none>".to_owned(), |locations| {
            locations
                .iter()
                .map(fmt_location)
                .collect::<Vec<_>>()
                .join("\n")
        });
        expected.assert_eq(&rendered);
    }

    const SLA: &str =
        "---\nid: s$0la\nkind: topic\ntitle: \"SLA\"\n---\n\nEvidence [@wright2004]. #anchor e1\n";
    const LMR: &str =
        "---\nid: lmr\nkind: topic\nrelated: [sla]\n---\n\nSee [sla](id:sla#e1). #mine\n";
    const ARG_SUPPORTS: &str = "---\nid: arg1\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: supports, op: analyze }\n  - { to: lmr, stance: supports }\n---\n";
    const ARG_OPPOSES: &str = "---\nid: arg2\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: opposes, op: recall }\n---\n";

    #[test]
    fn references_on_id_decl_collect_backlinks_grouped() {
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/lmr.md", LMR),
                // Note file order: opposes-arg before supports-arg, to show
                // the stance ordering is not insertion order.
                ("/v/arg2.md", ARG_OPPOSES),
                ("/v/arg1.md", ARG_SUPPORTS),
            ],
            true,
            expect![[r#"
                /v/sla.md 1:4..1:7
                /v/arg1.md 5:10..5:13
                /v/arg2.md 5:10..5:13
                /v/lmr.md 3:10..3:13
                /v/lmr.md 6:4..6:20"#]],
        );
    }

    #[test]
    fn references_without_declaration() {
        check(
            &[("/v/sla.md", SLA), ("/v/lmr.md", LMR)],
            false,
            expect![[r#"
                /v/lmr.md 3:10..3:13
                /v/lmr.md 6:4..6:20"#]],
        );
    }

    #[test]
    fn references_on_cite_find_citing_blocks() {
        check(
            &[
                (
                    "/v/sla.md",
                    "---\nid: sla\nkind: topic\n---\n\nEvidence [@wright$02004]. #anchor e1\n",
                ),
                (
                    "/v/other.md",
                    "---\nid: other\nkind: topic\n---\n\nAlso cites [@wright2004]. #anchor e2\n",
                ),
            ],
            false,
            expect![[r#"
                /v/sla.md 5:9..5:22
                /v/other.md 5:11..5:24"#]],
        );
    }

    #[test]
    fn references_in_prose_is_none() {
        check(
            &[(
                "/v/a.md",
                "---\nid: a\nkind: topic\n---\n\nPl$0ain. #mine\n",
            )],
            false,
            expect!["<none>"],
        );
    }
}

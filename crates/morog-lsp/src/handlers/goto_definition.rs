//! `textDocument/definition` (plan §5.6): id → file; `id#anchor` → the
//! block's range; `[@key]` → the source note if one exists, else the
//! bibliography entry directory.

use lsp_types::{GotoDefinitionParams, GotoDefinitionResponse, Location};

use crate::analysis::cursor::{self, CursorContext};
use crate::handlers::{HandlerError, anchor_block, file_location, file_position, location};
use crate::state::Snapshot;
use crate::to_proto;

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: GotoDefinitionParams,
) -> Result<Option<GotoDefinitionResponse>, HandlerError> {
    let Some(pos) = file_position(snapshot, &params.text_document_position_params) else {
        return Ok(None);
    };
    let target = match cursor::classify(&pos.fa, pos.offset) {
        CursorContext::LinkTarget {
            target_id, anchor, ..
        } => note_definition(snapshot, &target_id, anchor.as_deref()),
        CursorContext::Cite { key, .. } => cite_definition(snapshot, &key),
        _ => None,
    };
    Ok(target.map(GotoDefinitionResponse::Scalar))
}

fn note_definition(snapshot: &Snapshot, id: &str, anchor: Option<&str>) -> Option<Location> {
    let file = snapshot.analysis.resolve_id(id)?;
    if let Some(anchor) = anchor {
        let fa = snapshot.analysis.file(file)?;
        let block = anchor_block(fa, anchor)?;
        return location(snapshot, file, block.span);
    }
    file_location(snapshot, file)
}

/// `[@key]`: prefer a physical source note (its id equals the key); fall
/// back to the bibliography entry's directory (plan §4.2).
fn cite_definition(snapshot: &Snapshot, key: &str) -> Option<Location> {
    if let Some(file) = snapshot.analysis.resolve_id(key) {
        return file_location(snapshot, file);
    }
    let entry = snapshot.analysis.bib.get(key)?;
    if entry.dir.as_os_str().is_empty() {
        return None;
    }
    Some(Location {
        uri: to_proto::url(&entry.dir)?,
        range: lsp_types::Range::default(),
    })
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{PartialResultParams, WorkDoneProgressParams};

    use super::*;
    use crate::handlers::test_support::{fmt_location, snapshot};

    fn check(files: &[(&str, &str)], bib: &[&str], expected: Expect) {
        let (snap, position) = snapshot(files, bib);
        let response = handle(
            &snap,
            GotoDefinitionParams {
                text_document_position_params: position,
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
        .unwrap();
        let rendered = match response {
            Some(GotoDefinitionResponse::Scalar(location)) => fmt_location(&location),
            None => "<none>".to_owned(),
            other => format!("{other:?}"),
        };
        expected.assert_eq(&rendered);
    }

    const SLA: &str = "---\nid: sla\nkind: topic\ntitle: \"SLA\"\n---\n\nEvidence block [@wright2004]. #anchor e1\n";

    #[test]
    fn definition_of_id_is_the_id_declaration() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/a.md",
                    "---\nid: a\nkind: topic\n---\n\nSee [sla](id:sl$0a). #mine\n",
                ),
            ],
            &[],
            expect!["/v/sla.md 1:4..1:7"],
        );
    }

    #[test]
    fn definition_of_anchor_target_is_the_block() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/arg.md",
                    "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#e$01, stance: supports }\n---\n",
                ),
            ],
            &[],
            expect!["/v/sla.md 6:0..6:40"],
        );
    }

    #[test]
    fn definition_of_cite_prefers_the_source_note() {
        check(
            &[
                (
                    "/v/wright2004.md",
                    "---\nid: wright2004\nkind: source\n---\n",
                ),
                (
                    "/v/sla.md",
                    SLA.replacen("[@wright2004]", "[@wright$02004]", 1).leak(),
                ),
            ],
            &["wright2004"],
            expect!["/v/wright2004.md 1:4..1:14"],
        );
    }

    #[test]
    fn definition_of_cite_falls_back_to_the_bib_entry_dir() {
        check(
            &[(
                "/v/sla.md",
                SLA.replacen("[@wright2004]", "[@wright$02004]", 1).leak(),
            )],
            &["wright2004"],
            expect!["/bib/wright2004 0:0..0:0"],
        );
    }

    #[test]
    fn definition_of_unknown_is_none() {
        check(
            &[(
                "/v/a.md",
                "---\nid: a\nkind: topic\n---\n\nSee [x](id:no$0pe). #mine\n",
            )],
            &[],
            expect!["<none>"],
        );
    }
}

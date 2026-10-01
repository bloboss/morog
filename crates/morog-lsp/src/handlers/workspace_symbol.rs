//! `workspace/symbol` (plan §5.6): every note by id, title, and alias,
//! prefixed with its kind.

use lsp_types::{SymbolInformation, SymbolKind, WorkspaceSymbolParams, WorkspaceSymbolResponse};
use morog_sema::NoteKind;

use crate::handlers::{HandlerError, file_location, fuzzy_match};
use crate::state::Snapshot;

/// Cap the response; a fuzzier query narrows it (rust-analyzer's approach).
const LIMIT: usize = 256;

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: WorkspaceSymbolParams,
) -> Result<Option<WorkspaceSymbolResponse>, HandlerError> {
    let query = params.query.to_lowercase();

    let mut hits: Vec<(String, SymbolInformation)> = Vec::new();
    for (&file, fa) in &snapshot.analysis.files {
        let note = &fa.facts.note;
        let Some(id) = &note.id else { continue };
        let matches = fuzzy_match(&query, id.as_str())
            || note
                .title
                .as_deref()
                .is_some_and(|t| fuzzy_match(&query, t))
            || note.aliases.iter().any(|a| fuzzy_match(&query, a));
        if !matches {
            continue;
        }
        let Some(location) = file_location(snapshot, file) else {
            continue;
        };
        let kind = note.kind;
        let name = match kind {
            Some(kind) => format!("{} {id}", kind.as_str()),
            None => format!("note {id}"),
        };
        #[allow(deprecated)] // `deprecated` is a protocol field we must fill.
        let info = SymbolInformation {
            name,
            kind: symbol_kind(kind),
            tags: None,
            deprecated: None,
            location,
            container_name: note.title.clone(),
        };
        hits.push((id.as_str().to_owned(), info));
    }

    hits.sort_by(|a, b| a.0.cmp(&b.0));
    hits.truncate(LIMIT);
    Ok(Some(WorkspaceSymbolResponse::Flat(
        hits.into_iter().map(|(_, info)| info).collect(),
    )))
}

fn symbol_kind(kind: Option<NoteKind>) -> SymbolKind {
    match kind {
        Some(NoteKind::Source) => SymbolKind::FILE,
        Some(NoteKind::Topic) => SymbolKind::CLASS,
        Some(NoteKind::Argument) => SymbolKind::FUNCTION,
        None => SymbolKind::OBJECT,
    }
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{PartialResultParams, WorkDoneProgressParams};

    use super::*;
    use crate::handlers::test_support::{fmt_location, snapshot};

    const FILES: &[(&str, &str)] = &[
        (
            "/v/sla.md",
            "---\nid: s$0la\nkind: topic\ntitle: \"Specific leaf area\"\naliases: [SLA]\n---\n",
        ),
        (
            "/v/arg.md",
            "---\nid: allocation-arg\nkind: argument\ntitle: \"Allocation\"\nthesis: \"t\"\n---\n",
        ),
        ("/v/src.md", "---\nid: wright2004\nkind: source\n---\n"),
        ("/v/plain.md", "# No frontmatter\n"),
    ];

    fn check(query: &str, expected: Expect) {
        let (snap, _) = snapshot(FILES, &[]);
        let response = handle(
            &snap,
            WorkspaceSymbolParams {
                query: query.to_owned(),
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
            },
        )
        .unwrap();
        let rendered = match response {
            Some(WorkspaceSymbolResponse::Flat(symbols)) => symbols
                .iter()
                .map(|s| {
                    format!(
                        "{:?} {}{} @ {}",
                        s.kind,
                        s.name,
                        s.container_name
                            .as_deref()
                            .map(|c| format!(" ({c})"))
                            .unwrap_or_default(),
                        fmt_location(&s.location),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            other => format!("{other:?}"),
        };
        expected.assert_eq(&rendered);
    }

    #[test]
    fn empty_query_lists_every_note_kind_prefixed() {
        check(
            "",
            expect![[r#"
                Function argument allocation-arg (Allocation) @ /v/arg.md 1:4..1:18
                Class topic sla (Specific leaf area) @ /v/sla.md 1:4..1:7
                File source wright2004 @ /v/src.md 1:4..1:14"#]],
        );
    }

    #[test]
    fn query_matches_id_title_and_alias() {
        // "leaf" hits the topic's title; subsequence "alcarg" hits the id.
        check(
            "leaf",
            expect!["Class topic sla (Specific leaf area) @ /v/sla.md 1:4..1:7"],
        );
        check(
            "alcarg",
            expect!["Function argument allocation-arg (Allocation) @ /v/arg.md 1:4..1:18"],
        );
        check(
            "SLA",
            expect!["Class topic sla (Specific leaf area) @ /v/sla.md 1:4..1:7"],
        );
        check("nothing-matches-this", expect![""]);
    }
}

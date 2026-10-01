//! `textDocument/completion` (plan §5.6).
//!
//! - `FmKey`: keys valid for this note's `kind`.
//! - `FmValue`: closed enum values (`kind`, `status` per kind, `form`,
//!   `publish`).
//! - `RelationField::To` and `LinkTarget`: **topics and arguments only** —
//!   never sources, which enforces the STA direction — matched fuzzily on
//!   id, title, and aliases, with the title as detail. An `id#` partial
//!   completes the target note's block anchors.
//! - `RelationField::Stance` / `Op`: the closed enums.
//! - `Cite`: bibliography keys, with collection and year as detail.
//! - `Tag`: `#mine`, `#anchor`, the morg tag keywords, and the bibrat
//!   `TAGS.yml` vocabulary.
//!
//! Every item carries a text edit replacing exactly the typed partial, so
//! clients need no word-boundary guessing.

use std::collections::BTreeSet;

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionParams, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};
use morg_parser::LineIndex;
use morog_sema::{ArgumentForm, ArgumentStatus, BloomOp, NoteKind, SourceStatus, Stance};

use crate::analysis::cursor::{self, CompletionContext, RelField};
use crate::handlers::{HandlerError, file_position, fuzzy_match};
use crate::state::Snapshot;
use crate::to_proto;

/// Cap the open-ended item sources (targets, citekeys); a longer partial
/// narrows the list (the workspace-symbol approach, within the completion
/// budget of plan §5.7).
const LIMIT: usize = 256;

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: CompletionParams,
) -> Result<Option<CompletionResponse>, HandlerError> {
    let Some(pos) = file_position(snapshot, &params.text_document_position) else {
        return Ok(None);
    };
    let context = cursor::classify_completion(&pos.text, &pos.fa, pos.offset);
    let index = LineIndex::new(&pos.text);
    let range =
        |replace: morg_parser::Span| to_proto::range(&pos.text, &index, replace, snapshot.encoding);

    let items = match &context {
        CompletionContext::None => return Ok(None),
        CompletionContext::FmKey {
            kind,
            partial,
            replace,
        } => enum_items(
            &morog_sema::frontmatter_keys(*kind),
            partial,
            CompletionItemKind::FIELD,
            range(*replace),
        ),
        CompletionContext::FmValue {
            kind,
            key,
            partial,
            replace,
        } => {
            let values: &[&str] = match (key.as_str(), kind) {
                ("kind", _) => &NoteKind::ALL,
                ("status", Some(NoteKind::Source)) => &SourceStatus::ALL,
                ("status", Some(NoteKind::Argument)) => &ArgumentStatus::ALL,
                ("form", _) => &ArgumentForm::ALL,
                ("publish", _) => &["true", "false"],
                _ => &[],
            };
            enum_items(
                values,
                partial,
                CompletionItemKind::ENUM_MEMBER,
                range(*replace),
            )
        }
        CompletionContext::RelationField {
            field: RelField::Stance,
            partial,
            replace,
        } => enum_items(
            &Stance::ALL,
            partial,
            CompletionItemKind::ENUM_MEMBER,
            range(*replace),
        ),
        CompletionContext::RelationField {
            field: RelField::Op,
            partial,
            replace,
        } => enum_items(
            &BloomOp::ALL,
            partial,
            CompletionItemKind::ENUM_MEMBER,
            range(*replace),
        ),
        CompletionContext::RelationField {
            field: RelField::To,
            partial,
            replace,
        }
        | CompletionContext::LinkTarget { partial, replace } => {
            target_items(snapshot, partial, range(*replace))
        }
        CompletionContext::Cite { partial, replace } => {
            cite_items(snapshot, partial, range(*replace))
        }
        CompletionContext::Tag { partial, replace } => {
            tag_items(snapshot, partial, range(*replace))
        }
    };
    Ok(Some(CompletionResponse::Array(items)))
}

fn item(
    label: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
    range: Range,
) -> CompletionItem {
    CompletionItem {
        label: label.to_owned(),
        kind: Some(kind),
        detail,
        text_edit: Some(CompletionTextEdit::Edit(TextEdit {
            range,
            new_text: label.to_owned(),
        })),
        ..Default::default()
    }
}

/// Items for a closed vocabulary, filtered by the typed partial.
fn enum_items(
    values: &[&str],
    partial: &str,
    kind: CompletionItemKind,
    range: Range,
) -> Vec<CompletionItem> {
    let query = partial.to_lowercase();
    values
        .iter()
        .filter(|value| fuzzy_match(&query, value))
        .map(|value| item(value, kind, None, range))
        .collect()
}

/// Link and `to:` targets: topics and arguments only (plan §5.6 — never
/// sources, enforcing the STA direction).
fn target_items(snapshot: &Snapshot, partial: &str, range: Range) -> Vec<CompletionItem> {
    if let Some((id_part, anchor_part)) = partial.split_once('#') {
        return anchor_items(snapshot, id_part, anchor_part, range);
    }
    let query = partial.to_lowercase();
    let mut hits: Vec<(&str, Option<&str>, CompletionItemKind)> = Vec::new();
    for fa in snapshot.analysis.files.values() {
        let note = &fa.facts.note;
        let Some(id) = &note.id else { continue };
        let item_kind = match note.kind {
            Some(NoteKind::Topic) => CompletionItemKind::CLASS,
            Some(NoteKind::Argument) => CompletionItemKind::FUNCTION,
            Some(NoteKind::Source) | None => continue,
        };
        let matched = fuzzy_match(&query, id.as_str())
            || note
                .title
                .as_deref()
                .is_some_and(|t| fuzzy_match(&query, t))
            || note.aliases.iter().any(|a| fuzzy_match(&query, a));
        if matched {
            hits.push((id.as_str(), note.title.as_deref(), item_kind));
        }
    }
    hits.sort_by_key(|&(id, ..)| id);
    hits.dedup_by_key(|&mut (id, ..)| id);
    hits.truncate(LIMIT);
    hits.into_iter()
        .map(|(id, title, kind)| item(id, kind, title.map(str::to_owned), range))
        .collect()
}

/// Anchor completion once the target id is fixed: `sla#e` offers the block
/// anchors `sla` declares, replacing the whole `id#anchor` partial.
fn anchor_items(
    snapshot: &Snapshot,
    id_part: &str,
    anchor_part: &str,
    range: Range,
) -> Vec<CompletionItem> {
    let Some(fa) = snapshot
        .analysis
        .resolve_id(id_part)
        .and_then(|file| snapshot.analysis.file(file))
    else {
        return Vec::new();
    };
    let query = anchor_part.to_lowercase();
    fa.facts
        .blocks
        .iter()
        .filter_map(|block| block.anchor.as_deref())
        .filter(|anchor| fuzzy_match(&query, anchor))
        .map(|anchor| {
            item(
                &format!("{id_part}#{anchor}"),
                CompletionItemKind::REFERENCE,
                None,
                range,
            )
        })
        .collect()
}

/// Bibliography keys, with collection and year as detail (plan §5.6).
fn cite_items(snapshot: &Snapshot, partial: &str, range: Range) -> Vec<CompletionItem> {
    use morog_bib::BibStore;
    let query = partial.to_lowercase();
    let mut items: Vec<CompletionItem> = snapshot
        .analysis
        .bib
        .entries()
        .into_iter()
        .filter(|entry| fuzzy_match(&query, &entry.key.0))
        .map(|entry| {
            let mut detail = entry.collection.0.clone();
            if let Some(year) = entry.meta.as_ref().and_then(|meta| meta.year) {
                if detail.is_empty() {
                    detail = year.to_string();
                } else {
                    detail = format!("{detail} ({year})");
                }
            }
            item(
                &entry.key.0,
                CompletionItemKind::REFERENCE,
                (!detail.is_empty()).then_some(detail),
                range,
            )
        })
        .collect();
    items.sort_by(|a, b| a.label.cmp(&b.label));
    items.truncate(LIMIT);
    items
}

/// Tag names: `mine` and `anchor` first (the STA tags), then the remaining
/// morg keywords, then the user-declared `[tags]` names (plan §10 T1 —
/// custom tags join the keyword set), then the bibrat `TAGS.yml` vocabulary
/// (plan §5.6).
fn tag_items(snapshot: &Snapshot, partial: &str, range: Range) -> Vec<CompletionItem> {
    let query = partial.to_lowercase();
    let mut seen = BTreeSet::new();
    let mut names: Vec<&str> = vec!["mine", "anchor"];
    names.extend(
        morg_parser::tokens::Keyword::all()
            .iter()
            .map(|keyword| keyword.as_str()),
    );
    let mut custom: Vec<&str> = snapshot.analysis.tag_table.names().collect();
    custom.sort_unstable();
    names.extend(custom);
    names.extend(snapshot.analysis.bib_tags.iter().map(String::as_str));
    names
        .into_iter()
        .filter(|name| seen.insert(*name) && fuzzy_match(&query, name))
        .map(|name| item(name, CompletionItemKind::KEYWORD, None, range))
        .collect()
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};
    use lsp_types::{PartialResultParams, WorkDoneProgressParams};

    use super::*;
    use crate::handlers::test_support::snapshot;

    fn render(response: Option<CompletionResponse>) -> String {
        let Some(CompletionResponse::Array(items)) = response else {
            return "<none>".to_owned();
        };
        if items.is_empty() {
            return "<empty>".to_owned();
        }
        items
            .iter()
            .map(|item| {
                let Some(CompletionTextEdit::Edit(edit)) = &item.text_edit else {
                    panic!("every item carries a text edit");
                };
                format!(
                    "{:?} {}{} -> {}..{} `{}`",
                    item.kind.unwrap(),
                    item.label,
                    item.detail
                        .as_deref()
                        .map(|detail| format!(" ({detail})"))
                        .unwrap_or_default(),
                    crate::handlers::test_support::fmt_position(edit.range.start),
                    crate::handlers::test_support::fmt_position(edit.range.end),
                    edit.new_text,
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn run(snap: &Snapshot, position: lsp_types::TextDocumentPositionParams) -> String {
        render(
            handle(
                snap,
                CompletionParams {
                    text_document_position: position,
                    work_done_progress_params: WorkDoneProgressParams::default(),
                    partial_result_params: PartialResultParams::default(),
                    context: None,
                },
            )
            .unwrap(),
        )
    }

    fn check(files: &[(&str, &str)], bib: &[&str], expected: Expect) {
        let (snap, position) = snapshot(files, bib);
        expected.assert_eq(&run(&snap, position));
    }

    const SLA: &str = "---\nid: sla\nkind: topic\ntitle: \"Specific leaf area\"\naliases: [SLA]\n---\n\nEvidence [@wright2004]. #anchor e1\n\nMore [@reich1997]. #anchor e2\n";
    const SRC: &str = "---\nid: wright2004\nkind: source\n---\n";
    const ARG: &str =
        "---\nid: allocation-arg\nkind: argument\ntitle: \"Allocation\"\nthesis: \"t\"\n---\n";

    #[test]
    fn fm_keys_follow_the_kind() {
        check(
            &[(
                "/v/a.md",
                "---\nid: a\nkind: argument\nthesis: \"t\"\nst$0\n---\n",
            )],
            &[],
            expect!["Field status -> 4:0..4:2 `status`"],
        );
        // A topic has no `status` or `thesis`; `related` and `aliases` are
        // topic-only.
        check(
            &[("/v/a.md", "---\nid: a\nkind: topic\nrel$0\n---\n")],
            &[],
            expect!["Field related -> 3:0..3:3 `related`"],
        );
    }

    #[test]
    fn fm_values_offer_the_closed_enums() {
        check(
            &[("/v/a.md", "---\nid: a\nkind: to$0\n---\n")],
            &[],
            expect!["EnumMember topic -> 2:6..2:8 `topic`"],
        );
        check(
            &[("/v/a.md", "---\nid: a\nkind: source\nstatus: $0\n---\n")],
            &[],
            expect![[r#"
                EnumMember unread -> 3:8..3:8 `unread`
                EnumMember skimmed -> 3:8..3:8 `skimmed`
                EnumMember read -> 3:8..3:8 `read`
                EnumMember processed -> 3:8..3:8 `processed`"#]],
        );
        check(
            &[(
                "/v/a.md",
                "---\nid: a\nkind: argument\nthesis: \"t\"\nstatus: d$0\n---\n",
            )],
            &[],
            expect![[r#"
                EnumMember draft -> 4:8..4:9 `draft`
                EnumMember developing -> 4:8..4:9 `developing`"#]],
        );
    }

    #[test]
    fn relation_targets_are_topics_and_arguments_never_sources() {
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/src.md", SRC),
                ("/v/arg.md", ARG),
                (
                    "/v/new.md",
                    "---\nid: new\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: $0 }\n---\n",
                ),
            ],
            &["wright2004"],
            expect![[r#"
                Function allocation-arg (Allocation) -> 5:10..5:10 `allocation-arg`
                Function new -> 5:10..5:10 `new`
                Class sla (Specific leaf area) -> 5:10..5:10 `sla`"#]],
        );
    }

    #[test]
    fn targets_match_fuzzily_on_id_title_and_alias() {
        check(
            &[
                ("/v/sla.md", SLA),
                ("/v/arg.md", ARG),
                (
                    "/v/new.md",
                    "---\nid: new\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: leaf$0 }\n---\n",
                ),
            ],
            &[],
            // "leaf" hits the topic's title only.
            expect!["Class sla (Specific leaf area) -> 5:10..5:14 `sla`"],
        );
    }

    #[test]
    fn stance_and_op_offer_the_closed_enums() {
        check(
            &[(
                "/v/new.md",
                "---\nid: new\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: o$0 }\n---\n",
            )],
            &[],
            expect![[r#"
                EnumMember supports -> 5:23..5:24 `supports`
                EnumMember opposes -> 5:23..5:24 `opposes`"#]],
        );
        check(
            &[(
                "/v/new.md",
                "---\nid: new\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: supports, op: a$0 }\n---\n",
            )],
            &[],
            expect![[r#"
                EnumMember recall -> 5:37..5:38 `recall`
                EnumMember explain -> 5:37..5:38 `explain`
                EnumMember apply -> 5:37..5:38 `apply`
                EnumMember analyze -> 5:37..5:38 `analyze`
                EnumMember evaluate -> 5:37..5:38 `evaluate`
                EnumMember create -> 5:37..5:38 `create`"#]],
        );
    }

    #[test]
    fn anchor_partial_completes_the_targets_blocks() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/new.md",
                    "---\nid: new\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#$0 }\n---\n",
                ),
            ],
            &["wright2004", "reich1997"],
            expect![[r#"
                Reference sla#e1 -> 5:10..5:14 `sla#e1`
                Reference sla#e2 -> 5:10..5:14 `sla#e2`"#]],
        );
    }

    #[test]
    fn link_targets_complete_in_prose() {
        check(
            &[
                ("/v/sla.md", SLA),
                (
                    "/v/new.md",
                    "---\nid: new\nkind: topic\n---\n\nSee [link](id:sl$0\n",
                ),
            ],
            &[],
            expect!["Class sla (Specific leaf area) -> 5:14..5:16 `sla`"],
        );
    }

    #[test]
    fn cites_offer_bib_keys_with_collection_and_year() {
        check(
            &[(
                "/v/new.md",
                "---\nid: new\nkind: topic\n---\n\nEvidence [@wr$0\n",
            )],
            &["wright2004", "reich1997"],
            expect!["Reference wright2004 (fixtures (2020)) -> 5:11..5:13 `wright2004`"],
        );
    }

    #[test]
    fn tags_offer_sta_morg_and_bibrat_vocabulary() {
        let (mut snap, position) = snapshot(
            &[(
                "/v/new.md",
                "---\nid: new\nkind: topic\n---\n\nMy idea. #$0\n",
            )],
            &[],
        );
        snap.analysis.bib_tags = vec!["plastics".to_owned(), "mine".to_owned()].into();
        let rendered = run(&snap, position);
        // `mine` first and deduplicated against the bibrat vocabulary;
        // `anchor` and the morg keywords present; bibrat tags last.
        let labels: Vec<&str> = rendered
            .lines()
            .map(|line| line.split_whitespace().nth(1).unwrap())
            .collect();
        assert_eq!(labels[0], "mine");
        assert_eq!(labels[1], "anchor");
        assert!(labels.contains(&"todo") && labels.contains(&"deadline"));
        assert_eq!(*labels.last().unwrap(), "plastics");
        assert_eq!(labels.iter().filter(|l| **l == "mine").count(), 1);
    }

    #[test]
    fn tags_offer_user_declared_custom_names() {
        // A `[tags]` declaration joins the keyword set (plan §10 T1).
        let table = morg_parser::TagTable::build([morg_parser::TagDeclaration {
            name: "book".to_owned(),
            pattern: Some(r#""(?<title>[^"]+)"\s+by\s+(?<author>.+)"#.to_owned()),
            kind: None,
        }])
        .unwrap();
        let (snap, position) = crate::handlers::test_support::snapshot_with_tags(
            &[(
                "/v/new.md",
                "---\nid: new\nkind: topic\n---\n\nRead. #bo$0\n",
            )],
            &[],
            table,
        );
        expect!["Keyword book -> 5:7..5:9 `book`"].assert_eq(&run(&snap, position));
    }

    #[test]
    fn prose_yields_no_completion() {
        check(
            &[("/v/a.md", "---\nid: a\nkind: topic\n---\n\nPlain pro$0se\n")],
            &[],
            expect!["<none>"],
        );
    }
}

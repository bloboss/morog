//! Unit tests for `FileFacts` extraction and the file- and vault-level
//! lints (plan §2.4–2.5), driven by fixture strings through the public API.

use std::path::Path;

use morog_bib::{BibStore, FixtureStore};
use morog_sema::{
    Analysis, ArgumentForm, BlockUnit, BloomOp, LintCode, LintConfig, NoteKind, Severity, Stance,
    VaultNote, analyze_document, lint_file, lint_vault,
};

fn analyze(source: &str) -> Analysis {
    let result = morg_parser::parse_document(source);
    analyze_document(&result.document)
}

fn lint(source: &str) -> Vec<morog_sema::Diagnostic> {
    let analysis = analyze(source);
    lint_file(
        Path::new("test.md"),
        &analysis,
        &LintConfig::default(),
        None,
    )
}

fn codes(diags: &[morog_sema::Diagnostic]) -> Vec<LintCode> {
    diags.iter().map(|d| d.code).collect()
}

fn fixture_store(keys: &[&str]) -> FixtureStore {
    FixtureStore::from_keys(keys.iter().copied())
}

// ---------------------------------------------------------------------------
// Facts extraction
// ---------------------------------------------------------------------------

const TOPIC_NOTE: &str = "---\nid: specific-leaf-area\nkind: topic\ntitle: \"Specific leaf area\"\nrelated: [leaf-mass-ratio]\n---\n\n# Specific leaf area\n\nSLA is leaf area per dry mass [@wright_leaf_2004, p. 12]. #anchor evidence-1\n\nMy own hunch about scaling. #mine\n\nAn unsourced claim that violates provenance.\n\nSee also [leaf economics](id:leaf-economics#spectrum) for context. #mine\n";

#[test]
fn facts_classify_blocks_and_anchors() {
    let a = analyze(TOPIC_NOTE);
    let f = &a.facts;
    assert_eq!(f.note.kind, Some(NoteKind::Topic));
    assert_eq!(f.note.id.as_ref().unwrap().as_str(), "specific-leaf-area");

    // Heading + 4 paragraphs, blank lines skipped, indices contiguous.
    assert_eq!(f.blocks.len(), 5);
    assert_eq!(
        f.blocks.iter().map(|b| b.idx).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );

    let heading = &f.blocks[0];
    assert!(!heading.citable);
    assert_eq!(heading.unit, BlockUnit::None);

    let evidence = &f.blocks[1];
    assert_eq!(evidence.unit, BlockUnit::Evidence);
    assert_eq!(evidence.anchor.as_deref(), Some("evidence-1"));
    assert_eq!(evidence.cites.len(), 1);
    assert_eq!(evidence.cites[0].key.0, "wright_leaf_2004");
    assert_eq!(evidence.cites[0].locator.as_deref(), Some("p. 12"));
    let cite_span = evidence.cites[0].span;
    assert_eq!(
        &TOPIC_NOTE[cite_span.start..cite_span.end],
        "[@wright_leaf_2004, p. 12]"
    );

    assert_eq!(f.blocks[2].unit, BlockUnit::Mine);
    assert_eq!(f.blocks[3].unit, BlockUnit::None);
    assert!(f.blocks[3].citable);

    // id: link reference with anchor, attributed to its block.
    assert_eq!(f.id_refs.len(), 1);
    let r = &f.id_refs[0];
    assert_eq!(r.target_id, "leaf-economics");
    assert_eq!(r.target_anchor.as_deref(), Some("spectrum"));
    assert_eq!(r.block_idx, 4);

    // Declared related relation.
    assert_eq!(f.relations.len(), 1);
    assert_eq!(f.relations[0].target_id, "leaf-mass-ratio");

    assert!(f.body_words > 20);
}

#[test]
fn facts_collect_cites_from_list_items() {
    let src = "---\nid: t\nkind: topic\n---\n\n- first observation [@a_2020]\n- second observation [@b_2021, p. 3]\n";
    let a = analyze(src);
    assert_eq!(a.facts.blocks.len(), 1);
    let list = &a.facts.blocks[0];
    assert_eq!(list.unit, BlockUnit::Evidence);
    assert_eq!(list.cites.len(), 2);
    assert_eq!(list.cites[1].key.0, "b_2021");
}

#[test]
fn argument_facts_carry_relations_and_identity() {
    let src = "---\nid: an-arg\nkind: argument\nform: question\nthesis: \"Is it so?\"\nrelations:\n  - { to: a-topic, stance: supports, op: evaluate }\n  - { to: b-topic#x, stance: qualifies }\nstatus: developing\n---\n\nProse.\n";
    let a = analyze(src);
    let f = &a.facts;
    assert_eq!(f.note.form, Some(ArgumentForm::Question));
    assert_eq!(f.note.thesis.as_deref(), Some("Is it so?"));
    assert_eq!(f.note.status.as_deref(), Some("developing"));
    assert_eq!(f.relations.len(), 2);
    assert!(matches!(
        f.relations[0].kind,
        morog_sema::RelationKind::Argues {
            stance: Stance::Supports,
            op: Some(BloomOp::Evaluate)
        }
    ));
    // id span points at the id value.
    let span = f.note.id_span.unwrap();
    assert_eq!(&src[span.start..span.end], "an-arg");
}

// ---------------------------------------------------------------------------
// File-level lints
// ---------------------------------------------------------------------------

#[test]
fn lint_kind_unknown_value() {
    let diags = lint("---\nid: x\nkind: diary\n---\n");
    assert_eq!(codes(&diags), [LintCode::Kind]);
    assert_eq!(diags[0].severity, Severity::Error);
    let (line, col) = diags[0].line_col();
    assert_eq!(line, 3);
    assert!(col > 1);
}

#[test]
fn lint_kind_unknown_key() {
    let diags = lint("---\nid: x\nkind: source\nbogus: 1\n---\n");
    assert_eq!(codes(&diags), [LintCode::Kind]);
}

#[test]
fn lint_id_missing_and_not_slug() {
    let diags = lint("---\nkind: topic\n---\n\nA block. #mine\n");
    assert_eq!(codes(&diags), [LintCode::Id]);

    let diags = lint("---\nid: \"not a slug!\"\nkind: topic\n---\n\nA block. #mine\n");
    assert_eq!(codes(&diags), [LintCode::Id]);
    assert!(diags[0].message.contains("not a slug!"));
}

#[test]
fn lint_uncited_block_fires_only_for_topics() {
    let topic = "---\nid: t\nkind: topic\n---\n\nAn unsourced paragraph.\n";
    let diags = lint(topic);
    assert_eq!(codes(&diags), [LintCode::UncitedBlock]);
    assert_eq!(diags[0].severity, Severity::Warning);

    // Same body as an argument: no provenance rule.
    let arg = "---\nid: a\nkind: argument\nrelations:\n  - { to: x, stance: supports }\n  - { to: y, stance: opposes }\n---\n\nAn unsourced paragraph.\n";
    assert!(codes(&lint(arg)).is_empty());

    // Headings and code blocks are exempt.
    let exempt =
        "---\nid: t2\nkind: topic\n---\n\n# Heading\n\n```sh\nls\n```\n\nCited. [@k_2020]\n";
    assert!(codes(&lint(exempt)).is_empty());
}

#[test]
fn lint_topic_too_long_is_configurable() {
    let body: String = std::iter::repeat_n("word", 30)
        .collect::<Vec<_>>()
        .join(" ");
    let src = format!("---\nid: t\nkind: topic\n---\n\n{body} [@k_2020]\n");
    let analysis = analyze(&src);
    let small = LintConfig {
        topic_max_words: 10,
    };
    let diags = lint_file(Path::new("t.md"), &analysis, &small, None);
    assert_eq!(codes(&diags), [LintCode::TopicTooLong]);
    // Default (500) does not fire.
    let diags = lint_file(Path::new("t.md"), &analysis, &LintConfig::default(), None);
    assert!(diags.is_empty());
}

#[test]
fn lint_argument_arity() {
    let src = "---\nid: a\nkind: argument\nrelations:\n  - { to: x, stance: supports }\n---\n";
    let diags = lint(src);
    assert_eq!(codes(&diags), [LintCode::ArgumentArity]);
    assert_eq!(diags[0].severity, Severity::Warning);

    let src = "---\nid: a\nkind: argument\n---\n";
    assert_eq!(codes(&lint(src)), [LintCode::ArgumentArity]);
}

#[test]
fn lint_unknown_citekey_needs_a_store() {
    let src =
        "---\nid: t\nkind: topic\n---\n\nKnown fact [@known_2020]. Unknown fact [@ghost_1999].\n";
    let analysis = analyze(src);

    // Without a store the lint is skipped entirely.
    let diags = lint_file(Path::new("t.md"), &analysis, &LintConfig::default(), None);
    assert!(codes(&diags).is_empty());

    let store = fixture_store(&["known_2020"]);
    let diags = lint_file(
        Path::new("t.md"),
        &analysis,
        &LintConfig::default(),
        Some(&store as &dyn BibStore),
    );
    assert_eq!(codes(&diags), [LintCode::UnknownCitekey]);
    assert!(diags[0].message.contains("ghost_1999"));
    let span = diags[0].span.unwrap();
    assert_eq!(&src[span.start..span.end], "[@ghost_1999]");
}

#[test]
fn lint_source_id_must_match_bib_key() {
    let src = "---\nid: wrong_key_2020\nkind: source\n---\n";
    let analysis = analyze(src);
    let store = fixture_store(&["right_key_2020"]);
    let diags = lint_file(
        Path::new("s.md"),
        &analysis,
        &LintConfig::default(),
        Some(&store as &dyn BibStore),
    );
    assert_eq!(codes(&diags), [LintCode::Id]);
    // Without a store the check is skipped.
    let diags = lint_file(Path::new("s.md"), &analysis, &LintConfig::default(), None);
    assert!(diags.is_empty());
}

// ---------------------------------------------------------------------------
// Vault-level lints
// ---------------------------------------------------------------------------

struct Vault {
    files: Vec<(std::path::PathBuf, Analysis)>,
}

impl Vault {
    fn new(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(path, src)| (std::path::PathBuf::from(path), analyze(src)))
                .collect(),
        }
    }

    fn lint(&self) -> Vec<morog_sema::Diagnostic> {
        let notes: Vec<VaultNote<'_>> = self
            .files
            .iter()
            .map(|(path, a)| VaultNote {
                path,
                facts: &a.facts,
            })
            .collect();
        lint_vault(&notes)
    }
}

const GOOD_TOPIC: &str =
    "---\nid: squares\nkind: topic\n---\n\nFour sides. [@euclid_300bc] #anchor evidence-1\n";

#[test]
fn vault_lint_duplicate_ids() {
    let vault = Vault::new(&[
        ("a.md", "---\nid: dupe\nkind: topic\n---\n\nX. #mine\n"),
        ("b.md", "---\nid: dupe\nkind: argument\n---\n"),
        ("c.md", "---\nid: unique\nkind: topic\n---\n\nY. #mine\n"),
    ]);
    let diags = vault.lint();
    let dupes: Vec<_> = diags.iter().filter(|d| d.code == LintCode::Id).collect();
    assert_eq!(dupes.len(), 2);
    assert!(dupes.iter().all(|d| d.message.contains("dupe")));
}

#[test]
fn vault_lint_relation_targets() {
    let arg = "---\nid: my-arg\nkind: argument\nrelations:\n  - { to: squares, stance: supports }\n  - { to: nowhere, stance: opposes }\n  - { to: euclid_source, stance: supports }\n  - { to: squares#missing-anchor, stance: qualifies }\n---\n";
    let source = "---\nid: euclid_source\nkind: source\n---\n";
    let vault = Vault::new(&[
        ("arg.md", arg),
        ("squares.md", GOOD_TOPIC),
        ("src.md", source),
    ]);
    let diags = vault.lint();
    let targets: Vec<_> = diags
        .iter()
        .filter(|d| d.code == LintCode::RelationTarget)
        .collect();
    assert_eq!(targets.len(), 3, "{targets:?}");
    assert!(targets.iter().any(|d| d.message.contains("nowhere")));
    assert!(targets.iter().any(|d| d.message.contains("euclid_source")));
    assert!(targets.iter().any(|d| d.message.contains("missing-anchor")));
    // Spans point into the relations list.
    assert!(targets.iter().all(|d| d.span.is_some()));
}

#[test]
fn vault_lint_resolved_anchor_is_ok() {
    let arg = "---\nid: my-arg\nkind: argument\nrelations:\n  - { to: squares#evidence-1, stance: supports }\n  - { to: squares, stance: qualifies }\n---\n";
    let vault = Vault::new(&[("arg.md", arg), ("squares.md", GOOD_TOPIC)]);
    assert!(vault.lint().is_empty());
}

#[test]
fn vault_lint_positional_address() {
    // `#b2` with no such anchor declared: positional-address, an error.
    let arg = "---\nid: my-arg\nkind: argument\nrelations:\n  - { to: squares#b2, stance: supports }\n  - { to: gone#b1, stance: opposes }\n---\n";
    let vault = Vault::new(&[("arg.md", arg), ("squares.md", GOOD_TOPIC)]);
    let diags = vault.lint();
    let positional: Vec<_> = diags
        .iter()
        .filter(|d| d.code == LintCode::PositionalAddress)
        .collect();
    assert_eq!(positional.len(), 2, "{diags:?}");
    assert!(positional.iter().all(|d| d.severity == Severity::Error));

    // A declared anchor that happens to be named like `bN` is legitimate.
    let topic_with_b1 =
        "---\nid: squares\nkind: topic\n---\n\nFour sides. [@euclid_300bc] #anchor b1\n";
    let arg = "---\nid: my-arg\nkind: argument\nrelations:\n  - { to: squares#b1, stance: supports }\n  - { to: squares, stance: opposes }\n---\n";
    let vault = Vault::new(&[("arg.md", arg), ("squares.md", topic_with_b1)]);
    assert!(vault.lint().is_empty());
}

#[test]
fn vault_lint_pub_leak() {
    // A published topic and argument each target the private topic/source in
    // every way a note can: `related:`, inline `id:` links, inline cites,
    // and typed relations. Bib-only citekeys (no vault note) stay leak-free.
    let pub_topic = "---\nid: pub-topic\nkind: topic\npublish: true\nrelated: [private-topic]\n---\n\nCited [@private_source]. #anchor e1\n\nBib-only key [@bib_only_key]. #anchor e2\n\nSee [private](id:private-topic) and [public](id:pub-arg). #mine\n";
    let pub_arg = "---\nid: pub-arg\nkind: argument\npublish: true\nform: claim\nthesis: \"t\"\nrelations:\n  - { to: pub-topic#e1, stance: supports }\n  - { to: private-topic, stance: qualifies }\n---\n";
    // Private notes may target other private notes freely.
    let private_topic =
        "---\nid: private-topic\nkind: topic\n---\n\nAlso cites [@private_source]. #mine\n";
    let private_source = "---\nid: private_source\nkind: source\n---\n";
    let vault = Vault::new(&[
        ("pub-topic.md", pub_topic),
        ("pub-arg.md", pub_arg),
        ("private-topic.md", private_topic),
        ("private-source.md", private_source),
    ]);

    let diags = vault.lint();
    let leaks: Vec<_> = diags
        .iter()
        .filter(|d| d.code == LintCode::PubLeak)
        .collect();
    assert_eq!(leaks.len(), 4, "{leaks:?}");
    assert!(leaks.iter().all(|d| d.severity == Severity::Warning));
    assert!(leaks.iter().all(|d| d.span.is_some()));

    let messages: Vec<&str> = leaks.iter().map(|d| d.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("relates to private note `private-topic`")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("links to private note `private-topic`")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("cites private note `private_source`")),
        "{messages:?}"
    );
    // The argument's typed relation to the private topic leaks; its relation
    // to the published topic does not.
    assert_eq!(
        leaks
            .iter()
            .filter(|d| d.file.ends_with("pub-arg.md"))
            .count(),
        1
    );
    // Links between published notes and bib-only citekeys are leak-free, and
    // private notes never produce leaks.
    assert!(!messages.iter().any(|m| m.contains("bib_only_key")));
    assert!(!messages.iter().any(|m| m.contains("`pub-")));
    assert!(
        !leaks
            .iter()
            .any(|d| d.file.ends_with("private-topic.md") || d.file.ends_with("private-source.md"))
    );
}

// ---------------------------------------------------------------------------
// User-defined custom tags (plan §10 T1)
// ---------------------------------------------------------------------------

fn book_table() -> morg_parser::TagTable {
    morg_parser::TagTable::build([
        morg_parser::TagDeclaration {
            name: "book".to_owned(),
            pattern: Some(r#""(?<title>[^"]+)"\s+by\s+(?<author>.+)"#.to_owned()),
            kind: None,
        },
        morg_parser::TagDeclaration {
            name: "reading-time".to_owned(),
            pattern: None,
            kind: Some(morg_parser::CustomArgKind::Duration),
        },
    ])
    .unwrap()
}

fn analyze_with(source: &str, table: &morg_parser::TagTable) -> Analysis {
    let result = morg_parser::parse_document_with(source, table);
    analyze_document(&result.document)
}

#[test]
fn custom_tag_fields_surface_with_spans() {
    let source = "---\nid: t\nkind: topic\n---\n\nNotes [@k]. #book \"Dune\" by Frank Herbert\n\nQuick [@k]. #reading-time 1h30m\n";
    let a = analyze_with(source, &book_table());
    let blocks = &a.facts.blocks;

    let book = &blocks[0].customs[0];
    assert_eq!(book.name, "book");
    assert!(!book.shape_mismatch);
    let groups: Vec<&str> = book.fields.iter().map(|f| f.group.as_str()).collect();
    assert_eq!(groups, ["title", "author"]);
    // Pattern-group spans slice the source exactly.
    for field in &book.fields {
        assert_eq!(&source[field.span.start..field.span.end], field.value);
    }
    assert_eq!(book.fields[0].value, "Dune");
    assert_eq!(book.fields[1].value, "Frank Herbert");
    // The occurrence span covers the tag itself, for diagnostics.
    assert!(source[book.span.start..book.span.end].starts_with("#book"));

    // Kind shorthand: one field named after the kind, canonical rendering.
    let time = &blocks[1].customs[0];
    assert_eq!(time.name, "reading-time");
    assert_eq!(time.fields.len(), 1);
    assert_eq!(time.fields[0].group, "duration");
    assert_eq!(time.fields[0].value, "1h30m");
}

#[test]
fn shape_mismatch_is_a_warning_lint() {
    let source = "---\nid: t\nkind: topic\n---\n\nBad [@k]. #book untitled scrawl\n";
    let a = analyze_with(source, &book_table());
    let custom = &a.facts.blocks[0].customs[0];
    assert!(custom.shape_mismatch);
    assert!(custom.fields.is_empty());

    let diags = lint_file(Path::new("test.md"), &a, &LintConfig::default(), None);
    let mismatch: Vec<_> = diags
        .iter()
        .filter(|d| d.code == LintCode::TagShapeMismatch)
        .collect();
    assert_eq!(mismatch.len(), 1);
    assert_eq!(mismatch[0].severity, Severity::Warning);
    assert_eq!(mismatch[0].code.as_str(), "tag::shape-mismatch");
    // Wording matches `morg lint` exactly.
    assert_eq!(
        mismatch[0].message,
        "argument does not match the declared pattern for #book"
    );
    // The diagnostic points at the tag occurrence.
    let span = mismatch[0].span.unwrap();
    assert!(source[span.start..].starts_with("#book"));
}

#[test]
fn undeclared_tags_stay_unknown_without_a_table() {
    // The same source parsed without a table: no customs, no mismatch lint.
    let source = "---\nid: t\nkind: topic\n---\n\nBad [@k]. #book untitled scrawl\n";
    let a = analyze(source);
    assert!(a.facts.blocks[0].customs.is_empty());
    let diags = lint(source);
    assert!(!codes(&diags).contains(&LintCode::TagShapeMismatch));
}

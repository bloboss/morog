//! Unit tests for the typed frontmatter conversion (plan §2.1–2.3), driven
//! by fixture strings through the public API.

use morog_sema::{
    ArgumentForm, ArgumentStatus, BloomOp, MetaError, MetaErrorKind, NoteKind, NoteMeta,
    SourceStatus, Stance, parse_note_meta,
};

/// The typed view of one source string, for assertions.
struct Parsed {
    meta: NoteMeta,
    meta_errors: Vec<MetaError>,
}

fn analyze(source: &str) -> Parsed {
    let result = morg_parser::parse_document(source);
    let (meta, meta_errors) = parse_note_meta(result.document.frontmatter.as_ref(), source);
    Parsed { meta, meta_errors }
}

#[test]
fn source_meta_parses() {
    let src = "---\nid: paszke_pytorch_2019\nuuid: 0192f3c4-7a11-7bbb-8f55-111111111111\nkind: source\ntitle: \"Paszke 2019\"\nstatus: read\ncreated: 2026-09-30\n---\n\nBody.\n";
    let a = analyze(src);
    assert!(a.meta_errors.is_empty(), "{:?}", a.meta_errors);
    let NoteMeta::Source(m) = &a.meta else {
        panic!("expected source meta, got {:?}", a.meta);
    };
    assert_eq!(m.id.as_ref().unwrap().as_str(), "paszke_pytorch_2019");
    assert_eq!(
        m.uuid.unwrap().to_string(),
        "0192f3c4-7a11-7bbb-8f55-111111111111"
    );
    assert_eq!(m.title.as_deref(), Some("Paszke 2019"));
    assert_eq!(m.status, Some(SourceStatus::Read));
    assert_eq!(m.created.as_deref(), Some("2026-09-30"));
    assert!(!m.publish);
}

#[test]
fn topic_meta_parses() {
    let src = "---\nid: specific-leaf-area\nkind: topic\ntitle: \"Specific leaf area\"\naliases: [SLA]\nrelated: [leaf-mass-ratio]\ncreated: 2026-09-30\n---\n";
    let a = analyze(src);
    assert!(a.meta_errors.is_empty(), "{:?}", a.meta_errors);
    let NoteMeta::Topic(m) = &a.meta else {
        panic!("expected topic meta");
    };
    assert_eq!(m.aliases, ["SLA"]);
    assert_eq!(m.related.len(), 1);
    assert_eq!(m.related[0].to, "leaf-mass-ratio");
    // The related entry's span slices its own text out of the source.
    let span = m.related[0].span;
    assert_eq!(&src[span.start..span.end], "leaf-mass-ratio");
}

#[test]
fn argument_meta_parses_relations() {
    let src = "---\nid: two-shape-classes\nkind: argument\nform: claim\nthesis: \"Every planar shape is either square-like or round-like.\"\nrelations:\n  - { to: squares, stance: supports, op: analyze }\n  - { to: circles#evidence-2, stance: opposes }\nstatus: draft\npublish: true\n---\n";
    let a = analyze(src);
    assert!(a.meta_errors.is_empty(), "{:?}", a.meta_errors);
    let NoteMeta::Argument(m) = &a.meta else {
        panic!("expected argument meta");
    };
    assert_eq!(m.form, ArgumentForm::Claim);
    assert_eq!(m.status, Some(ArgumentStatus::Draft));
    assert!(m.publish);
    assert_eq!(m.relations.len(), 2);

    let r0 = &m.relations[0];
    assert_eq!(
        (r0.target_id.as_str(), r0.target_anchor.as_deref()),
        ("squares", None)
    );
    assert_eq!(r0.stance, Stance::Supports);
    assert_eq!(r0.op, Some(BloomOp::Analyze));
    assert_eq!(&src[r0.span.start..r0.span.end], "squares");

    let r1 = &m.relations[1];
    assert_eq!(r1.target_id, "circles");
    assert_eq!(r1.target_anchor.as_deref(), Some("evidence-2"));
    assert_eq!(r1.op, None);
    assert_eq!(&src[r1.span.start..r1.span.end], "circles#evidence-2");
}

#[test]
fn form_defaults_to_claim() {
    let src = "---\nid: an-arg\nkind: argument\nrelations:\n  - { to: a, stance: supports }\n  - { to: b, stance: opposes }\n---\n";
    let a = analyze(src);
    let NoteMeta::Argument(m) = &a.meta else {
        panic!("expected argument meta");
    };
    assert_eq!(m.form, ArgumentForm::Claim);
}

#[test]
fn no_kind_is_plain_note_without_errors() {
    // Non-STA keys are not validated on plain notes.
    let src = "---\ntags: [daily]\nwhatever: 42\n---\n\nJust prose.\n";
    let a = analyze(src);
    assert_eq!(a.meta, NoteMeta::Plain);
    assert!(a.meta_errors.is_empty());
    assert_eq!(a.meta.kind(), None);
}

#[test]
fn unknown_kind_is_error_with_value_span() {
    let src = "---\nid: x\nkind: journal\n---\n";
    let a = analyze(src);
    assert_eq!(a.meta, NoteMeta::Plain);
    assert_eq!(a.meta_errors.len(), 1);
    let err = &a.meta_errors[0];
    assert!(matches!(&err.kind, MetaErrorKind::UnknownKind { value } if value == "journal"));
    let span = err.span.expect("span");
    assert_eq!(&src[span.start..span.end], "journal");
}

#[test]
fn unknown_key_is_error_with_key_span() {
    let src = "---\nid: x\nkind: source\ncolor: green\n---\n";
    let a = analyze(src);
    let err = a
        .meta_errors
        .iter()
        .find(|e| matches!(&e.kind, MetaErrorKind::UnknownKey { key, .. } if key == "color"))
        .expect("unknown-key error");
    let span = err.span.expect("span");
    assert_eq!(&src[span.start..span.end], "color");
    // The note still types as a source.
    assert_eq!(a.meta.kind(), Some(NoteKind::Source));
}

#[test]
fn extra_map_is_exempt_from_unknown_keys() {
    let src = "---\nid: x\nkind: source\nextra:\n  rating: 5\n  shelf: desk\n---\n";
    let a = analyze(src);
    assert!(a.meta_errors.is_empty(), "{:?}", a.meta_errors);
    let NoteMeta::Source(m) = &a.meta else {
        panic!("expected source meta");
    };
    assert_eq!(m.extra.len(), 2);
    assert_eq!(m.extra[0].0, "rating");
}

#[test]
fn missing_id_and_bad_uuid_and_bad_status() {
    let src = "---\nkind: source\nuuid: not-a-uuid\nstatus: devoured\n---\n";
    let a = analyze(src);
    let kinds: Vec<_> = a.meta_errors.iter().map(|e| &e.kind).collect();
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, MetaErrorKind::MissingField { field } if field == "id"))
    );
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, MetaErrorKind::BadUuid { .. }))
    );
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, MetaErrorKind::BadEnumValue { field, .. } if field == "status"))
    );
}

#[test]
fn relation_errors_carry_spans() {
    let src = "---\nid: an-arg\nkind: argument\nrelations:\n  - { to: squares, stance: maybe }\n  - { stance: supports }\n  - { to: circles, stance: supports, weight: 3 }\n---\n";
    let a = analyze(src);
    let errs = &a.meta_errors;
    let bad_stance = errs
        .iter()
        .find(|e| matches!(&e.kind, MetaErrorKind::BadEnumValue { field, value, .. } if field == "relations[0].stance" && value == "maybe"))
        .expect("bad stance");
    let span = bad_stance.span.expect("span");
    assert_eq!(&src[span.start..span.end], "squares");
    assert!(errs.iter().any(
        |e| matches!(&e.kind, MetaErrorKind::MissingField { field } if field == "relations[1].to")
    ));
    assert!(errs.iter().any(
        |e| matches!(&e.kind, MetaErrorKind::UnknownKey { key, .. } if key == "relations[2].weight")
    ));

    // Only the well-formed relation (valid `to` and `stance`) survives.
    let NoteMeta::Argument(m) = &a.meta else {
        panic!("expected argument meta");
    };
    assert_eq!(m.relations.len(), 1);
    assert_eq!(m.relations[0].target_id, "circles");
}

#[test]
fn multibyte_content_keeps_spans_exact() {
    // Multi-byte characters before and inside the offending value.
    let src = "---\nid: caf\u{e9}-note\nkind: topic\ntitle: \"Caf\u{e9} \u{1f980} living\"\ncolour: \"caf\u{e9}\"\n---\n";
    let a = analyze(src);
    let err = a
        .meta_errors
        .iter()
        .find(|e| matches!(&e.kind, MetaErrorKind::UnknownKey { key, .. } if key == "colour"))
        .expect("unknown-key error");
    let span = err.span.expect("span");
    assert_eq!(&src[span.start..span.end], "colour");
    // Span line/col agree with a LineIndex over the same source.
    let index = morg_parser::LineIndex::new(src);
    let lc = index.line_col(span.start).unwrap();
    assert_eq!((lc.line, lc.col), (span.line, span.col));
    // The non-ASCII id is carried through (and will fail the slug lint).
    assert_eq!(a.meta.id().unwrap().as_str(), "caf\u{e9}-note");
}

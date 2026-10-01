//! Cursor-context classification (plan §5.5).
//!
//! Every positional request (hover, goto, references, …) first classifies
//! the offset it points at. L2 needs the navigation contexts — link targets,
//! citations, the note's own id — plus a minimal frontmatter key/value
//! split; the completion-oriented contexts (`RelationField`, `Tag`,
//! `partial` tracking) arrive with L3.
//!
//! Classification works off the extracted facts (whose spans the index
//! round-trips), falling back to the parse tree only for frontmatter
//! key/value shapes — so it also works on cold-start facts for everything
//! navigation needs.

use morg_parser::Span;

use crate::analysis::file::FileAnalysis;

/// What the cursor is on. Owned, so requests can carry it to worker threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorContext {
    /// A reference to a note: an inline `id:` link or a frontmatter
    /// `to:` / `related:` target.
    LinkTarget {
        target_id: String,
        anchor: Option<String>,
        span: Span,
    },
    /// An inline `[@key]` citation.
    Cite { key: String, span: Span },
    /// The note's own `id:` value in frontmatter.
    IdDecl { id: String, span: Span },
    /// A top-level frontmatter key.
    FmKey { key: String, span: Span },
    /// A top-level frontmatter value (not covered by a finer context).
    FmValue { key: String, span: Span },
    /// Anything else.
    Prose,
}

/// Classify `offset` (a UTF-8 byte offset) within one analyzed file.
pub fn classify(fa: &FileAnalysis, offset: usize) -> CursorContext {
    let facts = &fa.facts;

    // Inline `id:` links in prose.
    for id_ref in &facts.id_refs {
        if contains(id_ref.span, offset) {
            return CursorContext::LinkTarget {
                target_id: id_ref.target_id.clone(),
                anchor: id_ref.target_anchor.clone(),
                span: id_ref.span,
            };
        }
    }

    // Inline `[@key]` citations.
    for block in &facts.blocks {
        for cite in &block.cites {
            if contains(cite.span, offset) {
                return CursorContext::Cite {
                    key: cite.key.0.clone(),
                    span: cite.span,
                };
            }
        }
    }

    // Frontmatter `relations:` / `related:` targets.
    for rel in &facts.relations {
        if contains(rel.span, offset) {
            return CursorContext::LinkTarget {
                target_id: rel.target_id.clone(),
                anchor: rel.target_anchor.clone(),
                span: rel.span,
            };
        }
    }

    // The note's own id declaration.
    if let (Some(id), Some(span)) = (&facts.note.id, facts.note.id_span)
        && contains(span, offset)
    {
        return CursorContext::IdDecl {
            id: id.as_str().to_owned(),
            span,
        };
    }

    // Frontmatter keys and values, from the parse tree when present.
    if let Some(fm) = fa.doc.as_ref().and_then(|d| d.frontmatter.as_ref())
        && contains(fm.span, offset)
    {
        for entry in &fm.entries {
            if contains(entry.key_span, offset) {
                return CursorContext::FmKey {
                    key: entry.key.clone(),
                    span: entry.key_span,
                };
            }
            if contains(entry.value_span, offset) {
                return CursorContext::FmValue {
                    key: entry.key.clone(),
                    span: entry.value_span,
                };
            }
        }
    }

    CursorContext::Prose
}

/// Inclusive-end containment: a cursor sitting just past the last character
/// of a reference still counts, matching editor expectations.
fn contains(span: Span, offset: usize) -> bool {
    span.start <= offset && offset <= span.end
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use expect_test::{Expect, expect};
    use morog_sema::LintConfig;

    use super::*;

    /// Classify at the `$0` marker of a fixture (rust-analyzer style).
    fn check(fixture: &str, expected: Expect) {
        let offset = fixture.find("$0").expect("fixture has a $0 marker");
        let text = fixture.replacen("$0", "", 1);
        let fa = FileAnalysis::parse(
            Path::new("/vault/fixture.md"),
            text.into(),
            &LintConfig::default(),
            None,
        );
        expected.assert_debug_eq(&classify(&fa, offset));
    }

    #[test]
    fn classify_inline_link() {
        check(
            "---\nid: a\nkind: topic\n---\n\nSee [sla](id:s$0la#e1). #mine\n",
            expect![[r#"
                LinkTarget {
                    target_id: "sla",
                    anchor: Some(
                        "e1",
                    ),
                    span: Span {
                        start: 31,
                        end: 47,
                        line: 6,
                        col: 5,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn classify_cite() {
        check(
            "---\nid: a\nkind: topic\n---\n\nCited [@wright$02004, p. 4]. #anchor e1\n",
            expect![[r#"
                Cite {
                    key: "wright2004",
                    span: Span {
                        start: 33,
                        end: 52,
                        line: 6,
                        col: 7,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn classify_relation_target() {
        check(
            "---\nid: a\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sl$0a#e1, stance: supports }\n---\n",
            expect![[r#"
                LinkTarget {
                    target_id: "sla",
                    anchor: Some(
                        "e1",
                    ),
                    span: Span {
                        start: 58,
                        end: 64,
                        line: 6,
                        col: 11,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn classify_related_target() {
        check(
            "---\nid: a\nkind: topic\nrelated: [lm$0r]\n---\n",
            expect![[r#"
                LinkTarget {
                    target_id: "lmr",
                    anchor: None,
                    span: Span {
                        start: 32,
                        end: 35,
                        line: 4,
                        col: 11,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn classify_id_decl() {
        check(
            "---\nid: my-n$0ote\nkind: topic\n---\n",
            expect![[r#"
                IdDecl {
                    id: "my-note",
                    span: Span {
                        start: 8,
                        end: 15,
                        line: 2,
                        col: 5,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn classify_fm_key_and_value() {
        check(
            "---\nid: a\nki$0nd: topic\n---\n",
            expect![[r#"
                FmKey {
                    key: "kind",
                    span: Span {
                        start: 10,
                        end: 14,
                        line: 3,
                        col: 1,
                    },
                }
            "#]],
        );
        check(
            "---\nid: a\nkind: topic\ntitle: \"Lea$0ves\"\n---\n",
            expect![[r#"
                FmValue {
                    key: "title",
                    span: Span {
                        start: 29,
                        end: 37,
                        line: 4,
                        col: 8,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn classify_prose() {
        check(
            "---\nid: a\nkind: topic\n---\n\nPlain pro$0se here. #mine\n",
            expect![[r#"
                Prose
            "#]],
        );
    }

    #[test]
    fn classify_without_doc_still_navigates() {
        let text = "---\nid: a\nkind: topic\n---\n\nSee [sla](id:sla). #mine\n";
        let fa = FileAnalysis::parse(
            Path::new("/vault/fixture.md"),
            text.into(),
            &LintConfig::default(),
            None,
        );
        let offset = text.find("id:sla").unwrap() + 4;
        let facts_only = FileAnalysis::from_facts((*fa.facts).clone());
        match classify(&facts_only, offset) {
            CursorContext::LinkTarget { target_id, .. } => assert_eq!(target_id, "sla"),
            other => panic!("expected LinkTarget, got {other:?}"),
        }
    }
}

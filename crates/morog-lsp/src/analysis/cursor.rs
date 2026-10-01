//! Cursor-context classification (plan §5.5).
//!
//! Every positional request (hover, goto, references, …) first classifies
//! the offset it points at. Two classifiers live here:
//!
//! - [`classify`] — the navigation contexts (L2): link targets, citations,
//!   the note's own id, anchor declarations. It works off the extracted
//!   facts (whose spans the index round-trips), falling back to the parse
//!   tree only for frontmatter key/value shapes — so it also works on
//!   cold-start facts for everything navigation needs.
//! - [`classify_completion`] — the completion contexts (L3), with
//!   partial-prefix extraction. Completion runs mid-edit, when the facts and
//!   even the YAML parse may lag the buffer, so this classifier reads the
//!   line around the cursor instead of trusting extracted spans.

use morg_parser::Span;
use morog_sema::NoteKind;

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
    /// A `#anchor name` declaration in the body (`span` covers the whole
    /// tag). From the parse tree, so absent on facts-only entries.
    AnchorDecl { name: String, span: Span },
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

    // `#anchor name` declarations, from the parse tree when present.
    if let Some(doc) = fa.doc.as_ref()
        && let Some((name, span)) = anchor_decl_at(doc, offset)
    {
        return CursorContext::AnchorDecl { name, span };
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

/// The `#anchor name` tag containing `offset`, if any. Callout bodies are
/// skipped: their spans are relative to the parser's reassembled buffer.
fn anchor_decl_at(doc: &morg_parser::Document, offset: usize) -> Option<(String, Span)> {
    use morg_parser::{Block, InlineContent, InlineKind, Tag, TagKind};

    fn from_tag(tag: &Tag, offset: usize) -> Option<(String, Span)> {
        match &tag.kind {
            TagKind::Anchor { name } if contains(tag.span, offset) => {
                Some((name.clone(), tag.span))
            }
            _ => None,
        }
    }
    fn from_inline(content: &InlineContent, offset: usize) -> Option<(String, Span)> {
        content.segments.iter().find_map(|seg| match &seg.kind {
            InlineKind::Tag(tag) => from_tag(tag, offset),
            InlineKind::Bold(inner)
            | InlineKind::Italic(inner)
            | InlineKind::Strikethrough(inner) => from_inline(inner, offset),
            InlineKind::Link(link) => link.tags.iter().find_map(|tag| from_tag(tag, offset)),
            _ => None,
        })
    }
    fn from_block(block: &Block, offset: usize) -> Option<(String, Span)> {
        match block {
            Block::Paragraph(p) => from_inline(&p.content, offset),
            Block::Heading(h) => from_inline(&h.content, offset),
            Block::List(l) => l.items.iter().find_map(|item| {
                from_inline(&item.content, offset)
                    .or_else(|| {
                        item.description
                            .as_ref()
                            .and_then(|desc| from_inline(desc, offset))
                    })
                    .or_else(|| {
                        item.children
                            .iter()
                            .find_map(|child| from_block(child, offset))
                    })
            }),
            Block::BlockTag(tag) => from_tag(tag, offset),
            Block::CodeBlock(cb) => cb.tags.iter().find_map(|tag| from_tag(tag, offset)),
            Block::FootnoteDefinition(f) => from_inline(&f.content, offset),
            _ => None,
        }
    }
    doc.children
        .iter()
        .find_map(|block| from_block(block, offset))
}

// ---------------------------------------------------------------------------
// Completion classification (L3)

/// Which field of a `relations:` entry the cursor is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelField {
    To,
    Stance,
    Op,
}

/// What completion should offer at a position (plan §5.5's
/// completion-oriented contexts). `partial` is the prefix typed so far and
/// `replace` its span — what a completion's text edit replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionContext {
    /// A top-level frontmatter key position.
    FmKey {
        kind: Option<NoteKind>,
        partial: String,
        replace: Span,
    },
    /// A top-level frontmatter value position.
    FmValue {
        kind: Option<NoteKind>,
        key: String,
        partial: String,
        replace: Span,
    },
    /// Inside one field of a `relations:` entry (or a `related:` item,
    /// which completes like `to:`).
    RelationField {
        field: RelField,
        partial: String,
        replace: Span,
    },
    /// Inside the target of an inline `[text](id:…)` link.
    LinkTarget { partial: String, replace: Span },
    /// Inside an inline `[@…]` citation key.
    Cite { partial: String, replace: Span },
    /// Inside a `#…` tag name.
    Tag { partial: String, replace: Span },
    /// Nothing to complete here.
    None,
}

/// Classify `offset` (a UTF-8 byte offset) for completion.
///
/// Purely textual over the current line (plus the frontmatter bounds), so it
/// stays correct mid-edit; `fa` contributes only the note's kind.
pub fn classify_completion(text: &str, fa: &FileAnalysis, offset: usize) -> CompletionContext {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let line_start = text[..offset].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &text[line_start..offset];

    if in_frontmatter(text, offset) {
        classify_frontmatter(text, fa, offset, line_start, prefix)
    } else {
        classify_body(text, offset, prefix)
    }
}

/// Whether `offset` sits inside the frontmatter block. Textual (delimiter
/// scan), so it holds even while the YAML mid-edit no longer parses.
fn in_frontmatter(text: &str, offset: usize) -> bool {
    if !text.starts_with("---\n") || offset < 4 {
        return false;
    }
    match text[3..].find("\n---") {
        // Inside iff at or before the newline that opens the closing line.
        Some(close) => offset <= 3 + close,
        None => false,
    }
}

fn classify_frontmatter(
    text: &str,
    fa: &FileAnalysis,
    offset: usize,
    line_start: usize,
    prefix: &str,
) -> CompletionContext {
    // A half-typed line breaks the YAML parse and with it the extracted
    // kind, so fall back to reading the `kind:` line textually.
    let kind = fa.facts.note.kind.or_else(|| textual_kind(text));
    let indent_len = prefix.len() - prefix.trim_start().len();
    let body = &prefix[indent_len..];
    let body_start = line_start + indent_len;

    if indent_len == 0 {
        // A top-level `key: value` line (or a key still being typed).
        let Some(colon) = body.find(':') else {
            return CompletionContext::FmKey {
                kind,
                partial: body.to_owned(),
                replace: span_of(text, body_start, offset),
            };
        };
        let key = body[..colon].trim_end().to_owned();
        let after = &body[colon + 1..];
        if matches!(key.as_str(), "related" | "relations" | "aliases") {
            // A flow list on the key's own line: complete the current item
            // of `related:` (ids); `relations:` flow entries are classified
            // by the field scan below, `aliases` items are free text.
            if key == "related"
                && let Some(open) = after.rfind('[')
            {
                let item = &after[open + 1..];
                let sep = item.rfind(',').map_or(0, |i| i + 1);
                let partial = item[sep..].trim_start();
                return CompletionContext::RelationField {
                    field: RelField::To,
                    partial: partial.to_owned(),
                    replace: span_of(text, offset - partial.len(), offset),
                };
            }
            if key == "relations" {
                return classify_relation_entry(text, offset, after);
            }
            return CompletionContext::None;
        }
        let partial = after.trim_start();
        return CompletionContext::FmValue {
            kind,
            key,
            partial: partial.to_owned(),
            replace: span_of(text, offset - partial.len(), offset),
        };
    }

    // An indented line: an entry of a block list — the owning top-level key
    // decides what it completes as.
    match owning_key(text, line_start).as_deref() {
        Some("relations") => classify_relation_entry(text, offset, body),
        Some("related") => {
            let item = body.strip_prefix('-').map(str::trim_start).unwrap_or(body);
            CompletionContext::RelationField {
                field: RelField::To,
                partial: item.to_owned(),
                replace: span_of(text, offset - item.len(), offset),
            }
        }
        _ => CompletionContext::None,
    }
}

/// The note kind read straight off a `kind:` frontmatter line.
fn textual_kind(text: &str) -> Option<NoteKind> {
    let close = 3 + text.get(3..)?.find("\n---")?;
    text[..close]
        .lines()
        .find_map(|line| line.strip_prefix("kind:"))
        .and_then(|value| NoteKind::parse(value.trim()))
}

/// The nearest top-level (unindented) `key:` line above `line_start`.
fn owning_key(text: &str, mut line_start: usize) -> Option<String> {
    while line_start > 0 {
        let prev_end = line_start - 1; // the \n ending the previous line
        let prev_start = text[..prev_end].rfind('\n').map_or(0, |i| i + 1);
        let line = &text[prev_start..prev_end];
        if !line.starts_with([' ', '\t']) {
            return line.split(':').next().map(|k| k.trim().to_owned());
        }
        line_start = prev_start;
    }
    None
}

/// Classify within one `- { to: …, stance: …, op: … }` entry: the nearest
/// field key before the cursor wins, as long as the cursor is still inside
/// that field's value.
fn classify_relation_entry(text: &str, offset: usize, body: &str) -> CompletionContext {
    let mut best: Option<(usize, RelField, usize)> = None;
    for (name, field) in [
        ("to", RelField::To),
        ("stance", RelField::Stance),
        ("op", RelField::Op),
    ] {
        let mut from = 0;
        while let Some(found) = body[from..].find(name) {
            let pos = from + found;
            let before_ok =
                pos == 0 || matches!(body.as_bytes()[pos - 1], b' ' | b'\t' | b'{' | b',' | b'-');
            if before_ok && body[pos + name.len()..].starts_with(':') {
                let value_rel = pos + name.len() + 1;
                if best.is_none_or(|(p, ..)| pos > p) {
                    best = Some((pos, field, value_rel));
                }
            }
            from = pos + name.len();
        }
    }
    let Some((_, field, value_rel)) = best else {
        return CompletionContext::None;
    };
    let value = &body[value_rel..];
    if value.contains(',') || value.contains('}') {
        // Past this field: between entries, nothing to complete.
        return CompletionContext::None;
    }
    let partial = value.trim_start();
    CompletionContext::RelationField {
        field,
        partial: partial.to_owned(),
        replace: span_of(text, offset - partial.len(), offset),
    }
}

fn classify_body(text: &str, offset: usize, prefix: &str) -> CompletionContext {
    enum Marker {
        Cite,
        Link,
        Tag,
    }
    // The construct opening closest to the cursor wins.
    let cite = prefix.rfind("[@").map(|i| (i, Marker::Cite, i + 2));
    let link = prefix.rfind("(id:").map(|i| (i, Marker::Link, i + 4));
    let tag = prefix
        .rfind('#')
        .filter(|&i| {
            i == 0
                || prefix[..i]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
        })
        .map(|i| (i, Marker::Tag, i + 1));

    let best = [cite, link, tag]
        .into_iter()
        .flatten()
        .max_by_key(|&(pos, ..)| pos);
    let Some((_, marker, partial_rel)) = best else {
        return CompletionContext::None;
    };
    let partial = &prefix[partial_rel..];
    let replace = span_of(text, offset - partial.len(), offset);
    let slug_char = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    match marker {
        Marker::Cite if partial.chars().all(slug_char) => CompletionContext::Cite {
            partial: partial.to_owned(),
            replace,
        },
        Marker::Link if partial.chars().all(|c| slug_char(c) || c == '#') => {
            CompletionContext::LinkTarget {
                partial: partial.to_owned(),
                replace,
            }
        }
        Marker::Tag if partial.chars().all(slug_char) => CompletionContext::Tag {
            partial: partial.to_owned(),
            replace,
        },
        _ => CompletionContext::None,
    }
}

/// A span for `start..end`, with line/col derived from the text.
fn span_of(text: &str, start: usize, end: usize) -> Span {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line = text[..start].matches('\n').count() as u32 + 1;
    let col = (start - line_start) as u32 + 1;
    Span::new(start, end, line, col)
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
            &morg_parser::TagTable::empty(),
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
    fn classify_anchor_decl() {
        check(
            "---\nid: a\nkind: topic\n---\n\nCited [@w]. #anchor evid$0ence-1\n",
            expect![[r#"
                AnchorDecl {
                    name: "evidence-1",
                    span: Span {
                        start: 39,
                        end: 57,
                        line: 6,
                        col: 13,
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

    /// Classify for completion at the `$0` marker of a fixture.
    fn check_completion(fixture: &str, expected: Expect) {
        let offset = fixture.find("$0").expect("fixture has a $0 marker");
        let text = fixture.replacen("$0", "", 1);
        let fa = FileAnalysis::parse(
            Path::new("/vault/fixture.md"),
            text.as_str().into(),
            &LintConfig::default(),
            None,
            &morg_parser::TagTable::empty(),
        );
        expected.assert_debug_eq(&classify_completion(&text, &fa, offset));
    }

    #[test]
    fn complete_fm_key_and_value() {
        check_completion(
            "---\nid: a\nkind: topic\nti$0\n---\n",
            expect![[r#"
                FmKey {
                    kind: Some(
                        Topic,
                    ),
                    partial: "ti",
                    replace: Span {
                        start: 22,
                        end: 24,
                        line: 4,
                        col: 1,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: to$0\n---\n",
            expect![[r#"
                FmValue {
                    kind: None,
                    key: "kind",
                    partial: "to",
                    replace: Span {
                        start: 16,
                        end: 18,
                        line: 3,
                        col: 7,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: source\nstatus: $0\n---\n",
            expect![[r#"
                FmValue {
                    kind: Some(
                        Source,
                    ),
                    key: "status",
                    partial: "",
                    replace: Span {
                        start: 31,
                        end: 31,
                        line: 4,
                        col: 9,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn complete_relation_fields() {
        check_completion(
            "---\nid: a\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sl$0 }\n---\n",
            expect![[r#"
                RelationField {
                    field: To,
                    partial: "sl",
                    replace: Span {
                        start: 58,
                        end: 60,
                        line: 6,
                        col: 11,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: su$0 }\n---\n",
            expect![[r#"
                RelationField {
                    field: Stance,
                    partial: "su",
                    replace: Span {
                        start: 71,
                        end: 73,
                        line: 6,
                        col: 24,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla, stance: supports, op: $0 }\n---\n",
            expect![[r#"
                RelationField {
                    field: Op,
                    partial: "",
                    replace: Span {
                        start: 85,
                        end: 85,
                        line: 6,
                        col: 38,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn complete_related_items_as_targets() {
        check_completion(
            "---\nid: a\nkind: topic\nrelated: [lmr, sl$0]\n---\n",
            expect![[r#"
                RelationField {
                    field: To,
                    partial: "sl",
                    replace: Span {
                        start: 37,
                        end: 39,
                        line: 4,
                        col: 16,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: topic\nrelated:\n  - sl$0\n---\n",
            expect![[r#"
                RelationField {
                    field: To,
                    partial: "sl",
                    replace: Span {
                        start: 35,
                        end: 37,
                        line: 5,
                        col: 5,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn complete_body_constructs() {
        check_completion(
            "---\nid: a\nkind: topic\n---\n\nCited [@wri$0\n",
            expect![[r#"
                Cite {
                    partial: "wri",
                    replace: Span {
                        start: 35,
                        end: 38,
                        line: 6,
                        col: 9,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: topic\n---\n\nSee [sla](id:sla#e$0\n",
            expect![[r#"
                LinkTarget {
                    partial: "sla#e",
                    replace: Span {
                        start: 40,
                        end: 45,
                        line: 6,
                        col: 14,
                    },
                }
            "#]],
        );
        check_completion(
            "---\nid: a\nkind: topic\n---\n\nMy idea. #mi$0\n",
            expect![[r#"
                Tag {
                    partial: "mi",
                    replace: Span {
                        start: 37,
                        end: 39,
                        line: 6,
                        col: 11,
                    },
                }
            "#]],
        );
    }

    #[test]
    fn complete_nothing_in_plain_prose() {
        check_completion(
            "---\nid: a\nkind: topic\n---\n\nPlain pro$0se. #mine\n",
            expect![[r#"
                None
            "#]],
        );
        // A closed citation earlier in the line doesn't bleed through.
        check_completion(
            "---\nid: a\nkind: topic\n---\n\nCited [@w2004] and mo$0re\n",
            expect![[r#"
                None
            "#]],
        );
        // sla#e1 in prose is not a tag ('#' not preceded by whitespace).
        check_completion(
            "---\nid: a\nkind: topic\n---\n\nsee sla#e$0\n",
            expect![[r#"
                None
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
            &morg_parser::TagTable::empty(),
        );
        let offset = text.find("id:sla").unwrap() + 4;
        let facts_only = FileAnalysis::from_facts((*fa.facts).clone());
        match classify(&facts_only, offset) {
            CursorContext::LinkTarget { target_id, .. } => assert_eq!(target_id, "sla"),
            other => panic!("expected LinkTarget, got {other:?}"),
        }
    }
}

//! `FileFacts` extraction: reducing one parsed document to the plain,
//! index-ready facts (plan §3.2, §5.4).
//!
//! Extraction is pure (`&Document` + typed meta → [`FileFacts`]) and the
//! result is plain owned data — strings, numbers, spans — so the facts can
//! round-trip losslessly through simple rows (SQLite in M4) and be rebuilt
//! from read-only vault access.

use morg_parser::{Block, Document, InlineContent, InlineKind, Span, Tag, TagKind};

use crate::meta::NoteMeta;
use crate::types::{ArgumentForm, CiteKey, NoteId, NoteKind, NoteUuid, RelationKind};

/// The extracted, index-ready facts of a single vault file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileFacts {
    /// Note identity from the frontmatter.
    pub note: NoteFacts,
    /// Top-level content blocks (blank lines excluded), in document order.
    pub blocks: Vec<BlockFacts>,
    /// Relations declared in frontmatter (`relations:` and `related:`).
    pub relations: Vec<RelationFacts>,
    /// `id:` link references found in prose.
    pub id_refs: Vec<IdRef>,
    /// Word count of the body text (markup stripped; code excluded).
    pub body_words: u32,
}

/// Note identity, mirroring the index's `notes` row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteFacts {
    /// `None` for a plain morg note.
    pub kind: Option<NoteKind>,
    pub id: Option<NoteId>,
    /// Span of the `id:` value, for diagnostics.
    pub id_span: Option<Span>,
    pub uuid: Option<NoteUuid>,
    pub title: Option<String>,
    pub form: Option<ArgumentForm>,
    pub thesis: Option<String>,
    /// Normalized status string (`read`, `draft`, …), kind-specific.
    pub status: Option<String>,
    pub publish: bool,
    pub aliases: Vec<String>,
}

/// Classification of a block under the STA provenance rule (plan §2.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockUnit {
    /// Carries at least one `[@key]` citation.
    Evidence,
    /// Carries a `#mine` tag: the author's own idea.
    Mine,
    /// Neither — a provenance violation in a topic note.
    None,
}

impl BlockUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Mine => "mine",
            Self::None => "none",
        }
    }
}

/// One top-level block's facts, mirroring the index's `blocks` row.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockFacts {
    /// Positional index for internal bookkeeping — never a public address.
    pub idx: u32,
    /// Stable `#anchor` name, if the block declares one.
    pub anchor: Option<String>,
    pub unit: BlockUnit,
    /// Whether the provenance rule applies to this block kind
    /// (paragraph, list, callout — plan §2.4).
    pub citable: bool,
    /// Inline `[@key]` citations carried by the block.
    pub cites: Vec<CiteRef>,
    pub span: Span,
}

/// One inline citation.
#[derive(Debug, Clone, PartialEq)]
pub struct CiteRef {
    pub key: CiteKey,
    pub locator: Option<String>,
    pub span: Span,
}

/// One frontmatter-declared relation, mirroring the index's `relations` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationFacts {
    pub kind: RelationKind,
    /// Raw target text, e.g. `circles#evidence-2`.
    pub to: String,
    pub target_id: String,
    pub target_anchor: Option<String>,
    pub span: Span,
}

/// One `id:` link reference in prose.
#[derive(Debug, Clone, PartialEq)]
pub struct IdRef {
    pub target_id: String,
    pub target_anchor: Option<String>,
    /// Index of the top-level block containing the link.
    pub block_idx: u32,
    pub span: Span,
}

/// Extract the facts of one parsed document.
pub fn extract_facts(doc: &Document, meta: &NoteMeta) -> FileFacts {
    let note = note_facts(doc, meta);

    let mut blocks = Vec::new();
    let mut id_refs = Vec::new();
    let mut body_words: u32 = 0;
    let mut idx: u32 = 0;

    for block in &doc.children {
        if matches!(block, Block::BlankLine(_)) {
            continue;
        }
        let mut scan = BlockScan::default();
        scan_block(block, &mut scan);

        let citable = matches!(
            block,
            Block::Paragraph(_) | Block::List(_) | Block::Callout(_)
        );
        let unit = if !citable {
            BlockUnit::None
        } else if !scan.cites.is_empty() {
            BlockUnit::Evidence
        } else if scan.mine {
            BlockUnit::Mine
        } else {
            BlockUnit::None
        };

        id_refs.extend(scan.id_refs.into_iter().map(|(target, span)| {
            let (target_id, target_anchor) = crate::meta::split_target(&target);
            IdRef {
                target_id,
                target_anchor,
                block_idx: idx,
                span,
            }
        }));
        body_words += scan.words;

        blocks.push(BlockFacts {
            idx,
            anchor: scan.anchor,
            unit,
            citable,
            cites: scan.cites,
            span: block_span(block),
        });
        idx += 1;
    }

    FileFacts {
        note,
        blocks,
        relations: relation_facts(meta),
        id_refs,
        body_words,
    }
}

fn note_facts(doc: &Document, meta: &NoteMeta) -> NoteFacts {
    let id_span = doc
        .frontmatter
        .as_ref()
        .and_then(|fm| fm.entry("id"))
        .map(|e| e.value_span);
    let mut facts = NoteFacts {
        kind: meta.kind(),
        id_span,
        ..NoteFacts::default()
    };
    match meta {
        NoteMeta::Plain => {}
        NoteMeta::Source(m) => {
            facts.id = m.id.clone();
            facts.uuid = m.uuid;
            facts.title = m.title.clone();
            facts.status = m.status.map(|s| s.as_str().to_owned());
            facts.publish = m.publish;
        }
        NoteMeta::Topic(m) => {
            facts.id = m.id.clone();
            facts.uuid = m.uuid;
            facts.title = m.title.clone();
            facts.aliases = m.aliases.clone();
            facts.publish = m.publish;
        }
        NoteMeta::Argument(m) => {
            facts.id = m.id.clone();
            facts.uuid = m.uuid;
            facts.title = m.title.clone();
            facts.form = Some(m.form);
            facts.thesis = m.thesis.clone();
            facts.status = m.status.map(|s| s.as_str().to_owned());
            facts.publish = m.publish;
        }
    }
    facts
}

fn relation_facts(meta: &NoteMeta) -> Vec<RelationFacts> {
    match meta {
        NoteMeta::Argument(m) => m
            .relations
            .iter()
            .map(|r| RelationFacts {
                kind: RelationKind::Argues {
                    stance: r.stance,
                    op: r.op,
                },
                to: r.to.clone(),
                target_id: r.target_id.clone(),
                target_anchor: r.target_anchor.clone(),
                span: r.span,
            })
            .collect(),
        NoteMeta::Topic(m) => m
            .related
            .iter()
            .map(|r| {
                let (target_id, target_anchor) = crate::meta::split_target(&r.to);
                RelationFacts {
                    kind: RelationKind::Related,
                    to: r.to.clone(),
                    target_id,
                    target_anchor,
                    span: r.span,
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Accumulator for one top-level block's walk.
#[derive(Default)]
struct BlockScan {
    cites: Vec<CiteRef>,
    /// Raw `id:` targets with their spans.
    id_refs: Vec<(String, Span)>,
    anchor: Option<String>,
    mine: bool,
    words: u32,
}

impl BlockScan {
    fn tag(&mut self, tag: &Tag) {
        match &tag.kind {
            TagKind::Anchor { name } => {
                if self.anchor.is_none() {
                    self.anchor = Some(name.clone());
                }
            }
            TagKind::Unknown { name, .. } if name == "mine" => self.mine = true,
            _ => {}
        }
    }
}

fn scan_block(block: &Block, scan: &mut BlockScan) {
    match block {
        Block::Paragraph(p) => scan_inline(&p.content, scan),
        Block::Heading(h) => scan_inline(&h.content, scan),
        Block::List(l) => {
            for item in &l.items {
                scan_inline(&item.content, scan);
                if let Some(desc) = &item.description {
                    scan_inline(desc, scan);
                }
                for child in &item.children {
                    scan_block(child, scan);
                }
            }
        }
        Block::Callout(c) => {
            // Caveat: spans inside a callout body are relative to morg's
            // reassembled buffer, not the file (parser convention).
            for tag in &c.tags {
                scan.tag(tag);
            }
            for inner in &c.content {
                scan_block(inner, scan);
            }
        }
        Block::CodeBlock(cb) => {
            for tag in &cb.tags {
                scan.tag(tag);
            }
        }
        Block::BlockTag(tag) => scan.tag(tag),
        Block::Table(t) => {
            for cell in t.headers.iter().chain(t.rows.iter().flatten()) {
                scan_inline(cell, scan);
            }
        }
        Block::FootnoteDefinition(f) => scan_inline(&f.content, scan),
        Block::BlankLine(_)
        | Block::HorizontalRule(_)
        | Block::HtmlBlock(_)
        | Block::Comment(_) => {}
    }
}

fn scan_inline(content: &InlineContent, scan: &mut BlockScan) {
    for seg in &content.segments {
        match &seg.kind {
            InlineKind::Text(t) => scan.words += t.split_whitespace().count() as u32,
            InlineKind::Code(_) => {}
            InlineKind::Tag(tag) => scan.tag(tag),
            InlineKind::Bold(inner)
            | InlineKind::Italic(inner)
            | InlineKind::Strikethrough(inner) => scan_inline(inner, scan),
            InlineKind::Link(link) => {
                scan.words += link.text.split_whitespace().count() as u32;
                if let Some(target) = link.url.strip_prefix("id:") {
                    scan.id_refs.push((target.to_owned(), seg.span));
                }
                for tag in &link.tags {
                    scan.tag(tag);
                }
            }
            InlineKind::FootnoteRef(_) => {}
            InlineKind::Cite { key, locator } => scan.cites.push(CiteRef {
                key: CiteKey::new(key.clone()),
                locator: locator.clone(),
                span: seg.span,
            }),
        }
    }
}

fn block_span(block: &Block) -> Span {
    match block {
        Block::Heading(h) => h.span,
        Block::Paragraph(p) => p.span,
        Block::CodeBlock(c) => c.span,
        Block::BlankLine(s) | Block::HorizontalRule(s) => *s,
        Block::BlockTag(t) => t.span,
        Block::Callout(c) => c.span,
        Block::Table(t) => t.span,
        Block::HtmlBlock(h) => h.span,
        Block::List(l) => l.span,
        Block::Comment(c) => c.span,
        Block::FootnoteDefinition(f) => f.span,
    }
}

//! The STA lints (plan §2.5), split into a per-file pass and a vault-level
//! pass.
//!
//! File-level: `sta::kind`, the single-file parts of `sta::id` (missing /
//! not slug-shaped, plus source-id-matches-bibliography when a [`BibStore`]
//! is supplied), `sta::uncited-block`, `sta::topic-too-long`,
//! `sta::argument-arity`, and `sta::unknown-citekey` (skipped without a
//! store).
//!
//! Vault-level ([`lint_vault`]): `sta::id` uniqueness, `sta::relation-target`
//! (unresolved `to:`, argument → source), `sta::positional-address`, and
//! `pub::leak` (a `publish: true` note targeting a private note, plan §6.1 —
//! the exporter renders such targets as unlinked text).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use morg_parser::Span;
use morog_bib::BibStore;

use crate::Analysis;
use crate::facts::{BlockUnit, FileFacts};
use crate::types::{CiteKey, NoteKind, RelationKind};

/// Lint configuration.
#[derive(Debug, Clone)]
pub struct LintConfig {
    /// `sta::topic-too-long` fires above this body word count.
    pub topic_max_words: u32,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            topic_max_words: 500,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// The lint rules of plan §2.5 implemented in M2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LintCode {
    Kind,
    Id,
    UncitedBlock,
    TopicTooLong,
    ArgumentArity,
    RelationTarget,
    UnknownCitekey,
    PositionalAddress,
    PubLeak,
}

impl LintCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Kind => "sta::kind",
            Self::Id => "sta::id",
            Self::UncitedBlock => "sta::uncited-block",
            Self::TopicTooLong => "sta::topic-too-long",
            Self::ArgumentArity => "sta::argument-arity",
            Self::RelationTarget => "sta::relation-target",
            Self::UnknownCitekey => "sta::unknown-citekey",
            Self::PositionalAddress => "sta::positional-address",
            Self::PubLeak => "pub::leak",
        }
    }

    pub fn severity(self) -> Severity {
        match self {
            Self::Kind
            | Self::Id
            | Self::RelationTarget
            | Self::UnknownCitekey
            | Self::PositionalAddress => Severity::Error,
            Self::UncitedBlock | Self::TopicTooLong | Self::ArgumentArity | Self::PubLeak => {
                Severity::Warning
            }
        }
    }
}

/// One lint finding.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: LintCode,
    pub severity: Severity,
    pub message: String,
    pub file: PathBuf,
    /// `None` for whole-file findings; line/col then default to 1:1.
    pub span: Option<Span>,
}

impl Diagnostic {
    pub fn new(code: LintCode, message: String, file: &Path, span: Option<Span>) -> Self {
        Self {
            code,
            severity: code.severity(),
            message,
            file: file.to_path_buf(),
            span,
        }
    }

    /// 1-based line/column of the finding (1:1 when no span is attached).
    pub fn line_col(&self) -> (u32, u32) {
        self.span.map_or((1, 1), |s| (s.line, s.col))
    }
}

/// Run every file-local lint over one analyzed document.
///
/// `bib` enables the bibliography-dependent checks (`sta::unknown-citekey`
/// and the source-id half of `sta::id`); pass `None` to skip them.
pub fn lint_file(
    path: &Path,
    analysis: &Analysis,
    config: &LintConfig,
    bib: Option<&dyn BibStore>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let facts = &analysis.facts;
    let kind = facts.note.kind;

    // sta::kind / sta::id — schema violations from the typed conversion.
    for err in &analysis.meta_errors {
        let code = if err.is_id_error() {
            LintCode::Id
        } else {
            LintCode::Kind
        };
        out.push(Diagnostic::new(code, err.kind.to_string(), path, err.span));
    }

    // sta::id — shape of a declared id.
    if let Some(id) = &facts.note.id
        && !id.is_slug()
    {
        out.push(Diagnostic::new(
            LintCode::Id,
            format!(
                "id `{id}` is not slug-shaped (alphanumeric start, then alphanumerics, `-`, `_`)"
            ),
            path,
            facts.note.id_span,
        ));
    }

    // sta::id — a source id must equal its bibliography key.
    if kind == Some(NoteKind::Source)
        && let (Some(store), Some(id)) = (bib, &facts.note.id)
        && store.resolve(&CiteKey::new(id.as_str())).is_none()
    {
        out.push(Diagnostic::new(
            LintCode::Id,
            format!("source id `{id}` does not match any bibliography key"),
            path,
            facts.note.id_span,
        ));
    }

    if kind == Some(NoteKind::Topic) {
        // sta::uncited-block — the provenance rule (§2.4).
        for block in &facts.blocks {
            if block.citable && block.unit == BlockUnit::None {
                out.push(Diagnostic::new(
                    LintCode::UncitedBlock,
                    "topic block has neither a `[@key]` citation nor `#mine`".to_owned(),
                    path,
                    Some(block.span),
                ));
            }
        }

        // sta::topic-too-long.
        if facts.body_words > config.topic_max_words {
            out.push(Diagnostic::new(
                LintCode::TopicTooLong,
                format!(
                    "topic has {} words (limit {}); split it",
                    facts.body_words, config.topic_max_words
                ),
                path,
                None,
            ));
        }
    }

    // sta::argument-arity.
    if kind == Some(NoteKind::Argument) {
        let arity = facts
            .relations
            .iter()
            .filter(|r| matches!(r.kind, RelationKind::Argues { .. }))
            .count();
        if arity < 2 {
            out.push(Diagnostic::new(
                LintCode::ArgumentArity,
                format!("argument declares {arity} relation(s); an argument should connect at least two targets"),
                path,
                None,
            ));
        }
    }

    // sta::unknown-citekey — skipped when no bibliography store is supplied.
    if let Some(store) = bib {
        for block in &facts.blocks {
            for cite in &block.cites {
                if store.resolve(&cite.key).is_none() {
                    out.push(Diagnostic::new(
                        LintCode::UnknownCitekey,
                        format!("unknown citation key `@{}`", cite.key.0),
                        path,
                        Some(cite.span),
                    ));
                }
            }
        }
    }

    out
}

/// One vault file, as seen by the vault-level pass.
#[derive(Debug, Clone, Copy)]
pub struct VaultNote<'a> {
    pub path: &'a Path,
    pub facts: &'a FileFacts,
}

/// Run the cross-file lints: `sta::id` uniqueness, `sta::relation-target`,
/// `sta::positional-address`, and `pub::leak`.
pub fn lint_vault(notes: &[VaultNote<'_>]) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    // id → declaring notes.
    let mut by_id: HashMap<&str, Vec<&VaultNote<'_>>> = HashMap::new();
    for note in notes {
        if let Some(id) = &note.facts.note.id {
            by_id.entry(id.as_str()).or_default().push(note);
        }
    }

    // sta::id — uniqueness across the vault.
    for (id, holders) in &by_id {
        if holders.len() > 1 {
            for note in holders {
                let others: Vec<String> = holders
                    .iter()
                    .filter(|other| other.path != note.path)
                    .map(|other| other.path.display().to_string())
                    .collect();
                out.push(Diagnostic::new(
                    LintCode::Id,
                    format!("id `{id}` is also declared in {}", others.join(", ")),
                    note.path,
                    note.facts.note.id_span,
                ));
            }
        }
    }

    // Resolution map: id → (kind, publish, declared anchors).
    let resolve: HashMap<&str, (Option<NoteKind>, bool, Vec<&str>)> = by_id
        .iter()
        .map(|(id, holders)| {
            let first = holders[0];
            let anchors = first
                .facts
                .blocks
                .iter()
                .filter_map(|b| b.anchor.as_deref())
                .collect();
            (
                *id,
                (first.facts.note.kind, first.facts.note.publish, anchors),
            )
        })
        .collect();

    // sta::relation-target / sta::positional-address over argument relations.
    for note in notes {
        if note.facts.note.kind != Some(NoteKind::Argument) {
            continue;
        }
        for rel in &note.facts.relations {
            if !matches!(rel.kind, RelationKind::Argues { .. }) {
                continue;
            }
            let span = Some(rel.span);
            match resolve.get(rel.target_id.as_str()) {
                None => {
                    if rel.target_anchor.as_deref().is_some_and(is_positional) {
                        out.push(Diagnostic::new(
                            LintCode::PositionalAddress,
                            format!(
                                "relation targets positional address `{}`; use a stable `#anchor` name",
                                rel.to
                            ),
                            note.path,
                            span,
                        ));
                    } else {
                        out.push(Diagnostic::new(
                            LintCode::RelationTarget,
                            format!("relation target `{}` does not resolve to any note", rel.to),
                            note.path,
                            span,
                        ));
                    }
                }
                Some((kind, _publish, anchors)) => {
                    if *kind == Some(NoteKind::Source) {
                        out.push(Diagnostic::new(
                            LintCode::RelationTarget,
                            format!(
                                "argument relation targets source `{}`; arguments should go through topics",
                                rel.target_id
                            ),
                            note.path,
                            span,
                        ));
                    }
                    if let Some(anchor) = &rel.target_anchor
                        && !anchors.contains(&anchor.as_str())
                    {
                        if is_positional(anchor) {
                            out.push(Diagnostic::new(
                                LintCode::PositionalAddress,
                                format!(
                                    "relation targets positional address `{}` and `{}` declares no such anchor; use a stable `#anchor` name",
                                    rel.to, rel.target_id
                                ),
                                note.path,
                                span,
                            ));
                        } else {
                            out.push(Diagnostic::new(
                                LintCode::RelationTarget,
                                format!(
                                    "anchor `#{anchor}` is not declared in `{}`",
                                    rel.target_id
                                ),
                                note.path,
                                span,
                            ));
                        }
                    }
                }
            }
        }
    }

    // pub::leak — a `publish: true` note targeting a note that is not
    // published (plan §6.1). The exporter renders those targets as unlinked
    // text. A citation whose key has no vault note is a bibliography
    // virtual source (plan §4.2), which counts as publishable.
    for note in notes {
        if !note.facts.note.publish {
            continue;
        }
        let mut leak = |verb: &str, target: &str, span: Option<Span>| {
            out.push(Diagnostic::new(
                LintCode::PubLeak,
                format!(
                    "published note {verb} private note `{target}`; the exporter renders it as unlinked text"
                ),
                note.path,
                span,
            ));
        };
        let private =
            |target: &str| matches!(resolve.get(target), Some((_, published, _)) if !published);
        for rel in &note.facts.relations {
            if private(&rel.target_id) {
                leak("relates to", &rel.target_id, Some(rel.span));
            }
        }
        for id_ref in &note.facts.id_refs {
            if private(&id_ref.target_id) {
                leak("links to", &id_ref.target_id, Some(id_ref.span));
            }
        }
        for block in &note.facts.blocks {
            for cite in &block.cites {
                if private(&cite.key.0) {
                    leak("cites", &cite.key.0, Some(cite.span));
                }
            }
        }
    }

    out
}

/// Whether an anchor name looks like a removed positional `#bN` address.
fn is_positional(anchor: &str) -> bool {
    anchor
        .strip_prefix('b')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_anchor_shape() {
        assert!(is_positional("b1"));
        assert!(is_positional("b42"));
        assert!(!is_positional("b"));
        assert!(!is_positional("block-1"));
        assert!(!is_positional("evidence-2"));
        assert!(!is_positional("b1a"));
    }
}

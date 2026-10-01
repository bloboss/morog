//! The analysis snapshot (plan §5.2): an immutable, cheaply cloneable view
//! of everything the server has derived from the vault.
//!
//! Writes happen only on the main thread, which builds the next snapshot by
//! structural sharing and bumps `generation`; readers clone the
//! `Arc<Analysis>`-like value and run on worker threads. Diagnostics and
//! responses computed against an older generation are handled per plan §5.2.

pub mod cursor;
pub mod file;
pub mod xref;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use morog_sema::{Diagnostic, LintCode, NoteKind, RelationKind};

use crate::vfs::FileId;
pub use cursor::CursorContext;
pub use file::FileAnalysis;
pub use xref::{RelationRef, Xref};

/// In-memory bibliography: the `bib_entries` view of plan §4.2, loaded from
/// the index (never the filesystem) and replaced wholesale by the indexer.
#[derive(Debug, Default)]
pub struct BibIndex {
    entries: std::collections::HashMap<morog_bib::CiteKey, morog_bib::BibEntry>,
}

impl BibIndex {
    pub fn from_entries(entries: impl IntoIterator<Item = morog_bib::BibEntry>) -> Self {
        Self {
            entries: entries.into_iter().map(|e| (e.key.clone(), e)).collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&morog_bib::BibEntry> {
        self.entries.get(&morog_bib::CiteKey::new(key))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// `BibStore` so the in-memory table drives the bibliography-backed lints
/// (`sta::unknown-citekey`, the source-id half of `sta::id`).
impl morog_bib::BibStore for BibIndex {
    fn discover(&mut self, _vault_root: &Path) -> Result<(), morog_bib::BibError> {
        Ok(())
    }

    fn entries(&self) -> Vec<&morog_bib::BibEntry> {
        self.entries.values().collect()
    }

    fn resolve(&self, key: &morog_bib::CiteKey) -> Option<&morog_bib::BibEntry> {
        self.entries.get(key)
    }
}

/// The server's derived state, one immutable value (plan §5.2).
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// Per-file analyses.
    pub files: imbl::HashMap<FileId, Arc<FileAnalysis>>,
    /// Path of each known file (handlers turn these back into URIs).
    pub paths: imbl::HashMap<FileId, Arc<Path>>,
    /// The cross-file maps: `by_id`, `backrefs` (plan §5.2).
    pub xref: Xref,
    /// The bibliography table (plan §4.2).
    pub bib: Arc<BibIndex>,
    /// Bumped by the main thread on every write; stale readers compare it.
    pub generation: u64,
}

impl Analysis {
    /// Insert or replace one file's analysis, keeping the xref maps in sync.
    ///
    /// Returns every note id whose declaration or incoming references may
    /// have changed — the seed for [`Analysis::affected_files`].
    pub fn set_file(
        &mut self,
        file: FileId,
        path: Arc<Path>,
        new: Arc<FileAnalysis>,
    ) -> BTreeSet<Arc<str>> {
        let mut touched = BTreeSet::new();
        if let Some(old) = self.files.get(&file) {
            if let Some(id) = &old.facts.note.id {
                touched.insert(Arc::from(id.as_str()));
            }
            let old = Arc::clone(old);
            self.xref.remove_file(file, &old.facts);
        }
        if let Some(id) = &new.facts.note.id {
            touched.insert(Arc::from(id.as_str()));
        }
        self.xref.add_file(file, &new.facts);
        self.files.insert(file, new);
        self.paths.insert(file, path);
        touched
    }

    /// Remove one file entirely (deleted on disk).
    pub fn remove_file(&mut self, file: FileId) -> BTreeSet<Arc<str>> {
        let mut touched = BTreeSet::new();
        if let Some(old) = self.files.remove(&file) {
            if let Some(id) = &old.facts.note.id {
                touched.insert(Arc::from(id.as_str()));
            }
            self.xref.remove_file(file, &old.facts);
        }
        self.paths.remove(&file);
        touched
    }

    pub fn file(&self, file: FileId) -> Option<&Arc<FileAnalysis>> {
        self.files.get(&file)
    }

    pub fn path(&self, file: FileId) -> Option<&Arc<Path>> {
        self.paths.get(&file)
    }

    /// The file a note id resolves to.
    pub fn resolve_id(&self, id: &str) -> Option<FileId> {
        self.xref.resolve(id)
    }

    /// Files whose diagnostics may change when `ids` were touched: their
    /// declarers (id-uniqueness) and every file referencing them
    /// (relation-target, anchors).
    pub fn affected_files(&self, ids: &BTreeSet<Arc<str>>) -> BTreeSet<FileId> {
        let mut out = BTreeSet::new();
        for id in ids {
            if let Some(holders) = self.xref.by_id.get(&**id) {
                out.extend(holders.iter().copied());
            }
            out.extend(self.xref.refs_to(id).map(|r| r.file));
        }
        out
    }

    /// The vault-level lints for one file, recomputed from the xref maps
    /// (plan M7 L1): `sta::id` uniqueness, `sta::relation-target`, and
    /// `sta::positional-address`. Mirrors `morog_sema::lint_vault`, scoped
    /// to a single file so a keystroke never relints the whole vault.
    pub fn cross_diags(&self, file: FileId) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        let Some(fa) = self.files.get(&file) else {
            return out;
        };
        let Some(path) = self.paths.get(&file) else {
            return out;
        };
        let facts = &fa.facts;

        // sta::id — uniqueness across the vault.
        if let Some(id) = &facts.note.id
            && let Some(holders) = self.xref.by_id.get(id.as_str())
            && holders.len() > 1
        {
            let others: Vec<String> = holders
                .iter()
                .filter(|&&f| f != file)
                .filter_map(|f| self.paths.get(f))
                .map(|p| p.display().to_string())
                .collect();
            out.push(Diagnostic::new(
                LintCode::Id,
                format!("id `{id}` is also declared in {}", others.join(", ")),
                path,
                facts.note.id_span,
            ));
        }

        // sta::relation-target / sta::positional-address over argument
        // relations (same rules and wording as `lint_vault`).
        if facts.note.kind == Some(NoteKind::Argument) {
            for rel in &facts.relations {
                if !matches!(rel.kind, RelationKind::Argues { .. }) {
                    continue;
                }
                let span = Some(rel.span);
                match self.xref.resolve(&rel.target_id) {
                    None => {
                        if rel.target_anchor.as_deref().is_some_and(is_positional) {
                            out.push(Diagnostic::new(
                                LintCode::PositionalAddress,
                                format!(
                                    "relation targets positional address `{}`; use a stable `#anchor` name",
                                    rel.to
                                ),
                                path,
                                span,
                            ));
                        } else {
                            out.push(Diagnostic::new(
                                LintCode::RelationTarget,
                                format!(
                                    "relation target `{}` does not resolve to any note",
                                    rel.to
                                ),
                                path,
                                span,
                            ));
                        }
                    }
                    Some(target_file) => {
                        let target = self.files.get(&target_file);
                        let target_kind = target.and_then(|t| t.facts.note.kind);
                        if target_kind == Some(NoteKind::Source) {
                            out.push(Diagnostic::new(
                                LintCode::RelationTarget,
                                format!(
                                    "argument relation targets source `{}`; arguments should go through topics",
                                    rel.target_id
                                ),
                                path,
                                span,
                            ));
                        }
                        if let Some(anchor) = &rel.target_anchor
                            && !target.is_some_and(|t| {
                                t.facts
                                    .blocks
                                    .iter()
                                    .any(|b| b.anchor.as_deref() == Some(anchor.as_str()))
                            })
                        {
                            if is_positional(anchor) {
                                out.push(Diagnostic::new(
                                    LintCode::PositionalAddress,
                                    format!(
                                        "relation targets positional address `{}` and `{}` declares no such anchor; use a stable `#anchor` name",
                                        rel.to, rel.target_id
                                    ),
                                    path,
                                    span,
                                ));
                            } else {
                                out.push(Diagnostic::new(
                                    LintCode::RelationTarget,
                                    format!(
                                        "anchor `#{anchor}` is not declared in `{}`",
                                        rel.target_id
                                    ),
                                    path,
                                    span,
                                ));
                            }
                        }
                    }
                }
            }
        }

        out
    }
}

/// Whether an anchor name looks like a removed positional `#bN` address
/// (plan §2.4; same shape as `morog_sema::lint`).
fn is_positional(anchor: &str) -> bool {
    anchor
        .strip_prefix('b')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use morog_sema::LintConfig;

    use super::*;

    fn set(analysis: &mut Analysis, file: FileId, path: &str, text: &str) -> BTreeSet<Arc<str>> {
        let path: Arc<Path> = Arc::from(Path::new(path).to_path_buf());
        let fa = Arc::new(FileAnalysis::parse(
            &path,
            text.into(),
            &LintConfig::default(),
            None,
        ));
        analysis.set_file(file, path, fa)
    }

    const SLA: &str = "---\nid: sla\nkind: topic\n---\n\nCited [@wright2004]. #anchor e1\n";
    const ARG_OK: &str = "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#e1, stance: supports }\n  - { to: sla, stance: opposes }\n---\n";
    const ARG_BAD: &str = "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: missing, stance: supports }\n  - { to: sla#b2, stance: opposes }\n  - { to: sla#nope, stance: opposes }\n  - { to: src, stance: qualifies }\n---\n";

    #[test]
    fn cross_diags_resolve_relations() {
        let mut analysis = Analysis::default();
        set(&mut analysis, FileId(0), "/v/sla.md", SLA);
        set(&mut analysis, FileId(1), "/v/arg.md", ARG_OK);
        assert!(analysis.cross_diags(FileId(0)).is_empty());
        assert!(analysis.cross_diags(FileId(1)).is_empty());

        set(
            &mut analysis,
            FileId(2),
            "/v/src.md",
            "---\nid: src\nkind: source\n---\n",
        );
        set(&mut analysis, FileId(1), "/v/arg.md", ARG_BAD);
        let diags = analysis.cross_diags(FileId(1));
        let codes: Vec<&str> = diags.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                "sta::relation-target",    // missing
                "sta::positional-address", // sla#b2
                "sta::relation-target",    // sla#nope
                "sta::relation-target",    // src is a source
            ]
        );
    }

    #[test]
    fn cross_diags_report_duplicate_ids_on_both_files() {
        let mut analysis = Analysis::default();
        set(&mut analysis, FileId(0), "/v/a.md", SLA);
        let touched = set(&mut analysis, FileId(1), "/v/b.md", SLA);
        assert_eq!(touched.iter().map(|s| &**s).collect::<Vec<_>>(), ["sla"]);

        let affected = analysis.affected_files(&touched);
        assert!(affected.contains(&FileId(0)) && affected.contains(&FileId(1)));
        for (file, other) in [(FileId(0), "/v/b.md"), (FileId(1), "/v/a.md")] {
            let diags = analysis.cross_diags(file);
            assert_eq!(diags.len(), 1, "{file:?}");
            assert_eq!(diags[0].code, LintCode::Id);
            assert!(diags[0].message.contains(other));
        }

        // Fixing one file clears the other.
        let touched = set(
            &mut analysis,
            FileId(1),
            "/v/b.md",
            "---\nid: sla2\nkind: topic\n---\n\nCited [@x]. #anchor e1\n",
        );
        assert_eq!(touched.len(), 2); // sla and sla2
        assert!(analysis.cross_diags(FileId(0)).is_empty());
        assert!(analysis.cross_diags(FileId(1)).is_empty());
    }

    #[test]
    fn affected_files_follow_backrefs() {
        let mut analysis = Analysis::default();
        set(&mut analysis, FileId(0), "/v/sla.md", SLA);
        set(&mut analysis, FileId(1), "/v/arg.md", ARG_OK);

        // Deleting the topic leaves the argument dangling and affected.
        let touched = analysis.remove_file(FileId(0));
        let affected = analysis.affected_files(&touched);
        assert!(affected.contains(&FileId(1)));
        let diags = analysis.cross_diags(FileId(1));
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.code == LintCode::RelationTarget));
    }

    #[test]
    fn bib_index_resolves_keys() {
        use morog_bib::BibStore;
        let bib = BibIndex::from_entries([morog_bib::BibEntry::minimal("wright2004")]);
        assert_eq!(bib.len(), 1);
        assert!(!bib.is_empty());
        assert!(bib.get("wright2004").is_some());
        assert!(bib.get("missing").is_none());
        assert!(
            bib.resolve(&morog_bib::CiteKey::new("wright2004"))
                .is_some()
        );
    }
}

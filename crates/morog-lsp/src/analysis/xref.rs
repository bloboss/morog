//! Cross-file reference maps (plan §5.1–5.2): which file declares an id,
//! and which files point at it.
//!
//! The maps are `imbl` persistent structures, so cloning the whole
//! [`Xref`] into a snapshot is O(1) and updates on the main thread share
//! structure with in-flight readers.
//!
//! Three reference kinds land in `backrefs`:
//! - frontmatter `relations:` / `related:` entries (argues / related),
//! - inline `id:` links in prose (mentions),
//! - inline `[@key]` citations (cites), keyed by the citekey — which equals
//!   the source note id when one exists, so a source note's backlinks
//!   include the blocks citing it.

use std::sync::Arc;

use imbl::{HashMap, Vector};
use morg_parser::Span;
use morog_sema::{FileFacts, RelationKind};

use crate::vfs::FileId;

/// One reference to a target id (or citekey), as stored in `backrefs`.
#[derive(Debug, Clone, PartialEq)]
pub struct RelationRef {
    /// The file the reference appears in.
    pub file: FileId,
    pub kind: RelationKind,
    pub target_id: Arc<str>,
    pub target_anchor: Option<Arc<str>>,
    /// Span of the reference in `file` (the `to:` value, link segment, or
    /// citation).
    pub span: Span,
}

/// The cross-file maps: id → declaring files, target → incoming references.
#[derive(Debug, Clone, Default)]
pub struct Xref {
    /// Declared note id → files declaring it, in insertion order. More than
    /// one entry is the `sta::id` uniqueness violation; resolution uses the
    /// first.
    pub by_id: HashMap<Arc<str>, Vector<FileId>>,
    /// Target id (or citekey) → references pointing at it, resolved or not.
    /// The unresolved view of plan §5.1 is the subset whose key is absent
    /// from `by_id` (see [`Xref::is_unresolved`]).
    pub backrefs: HashMap<Arc<str>, Vector<RelationRef>>,
}

impl Xref {
    /// Record one file's declarations and references.
    pub fn add_file(&mut self, file: FileId, facts: &FileFacts) {
        if let Some(id) = &facts.note.id {
            let id: Arc<str> = Arc::from(id.as_str());
            self.by_id.entry(id).or_default().push_back(file);
        }
        for r in file_refs(file, facts) {
            self.backrefs
                .entry(Arc::clone(&r.target_id))
                .or_default()
                .push_back(r);
        }
    }

    /// Remove one file's declarations and references (`facts` must be the
    /// same facts that were added).
    pub fn remove_file(&mut self, file: FileId, facts: &FileFacts) {
        if let Some(id) = &facts.note.id {
            let id: &str = id.as_str();
            if let Some(holders) = self.by_id.get_mut(id) {
                holders.retain(|&f| f != file);
                if holders.is_empty() {
                    self.by_id.remove(id);
                }
            }
        }
        // Collect the touched targets first; retain + drop empty entries.
        let targets: std::collections::BTreeSet<Arc<str>> =
            file_refs(file, facts).map(|r| r.target_id).collect();
        for target in targets {
            if let Some(refs) = self.backrefs.get_mut(&target) {
                refs.retain(|r| r.file != file);
                if refs.is_empty() {
                    self.backrefs.remove(&target);
                }
            }
        }
    }

    /// The file an id resolves to: the first declarer.
    pub fn resolve(&self, id: &str) -> Option<FileId> {
        self.by_id.get(id).and_then(|v| v.front()).copied()
    }

    /// All incoming references for a target id or citekey.
    pub fn refs_to(&self, target: &str) -> impl Iterator<Item = &RelationRef> {
        self.backrefs.get(target).into_iter().flatten()
    }

    /// Whether a referenced target has no declaring note (the `unresolved`
    /// map of plan §5.1, expressed as a predicate over `backrefs`).
    pub fn is_unresolved(&self, target: &str) -> bool {
        !self.by_id.contains_key(target) && self.backrefs.contains_key(target)
    }
}

/// Every outgoing reference of one file, in document order.
fn file_refs(file: FileId, facts: &FileFacts) -> impl Iterator<Item = RelationRef> + '_ {
    let relations = facts.relations.iter().map(move |rel| RelationRef {
        file,
        kind: rel.kind,
        target_id: Arc::from(rel.target_id.as_str()),
        target_anchor: rel.target_anchor.as_deref().map(Arc::from),
        span: rel.span,
    });
    let mentions = facts.id_refs.iter().map(move |id_ref| RelationRef {
        file,
        kind: RelationKind::Mentions,
        target_id: Arc::from(id_ref.target_id.as_str()),
        target_anchor: id_ref.target_anchor.as_deref().map(Arc::from),
        span: id_ref.span,
    });
    let cites = facts.blocks.iter().flat_map(move |block| {
        block.cites.iter().map(move |cite| RelationRef {
            file,
            kind: RelationKind::Cites,
            target_id: Arc::from(cite.key.0.as_str()),
            target_anchor: None,
            span: cite.span,
        })
    });
    relations.chain(mentions).chain(cites)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use morog_sema::LintConfig;

    use super::*;
    use crate::analysis::file::FileAnalysis;

    fn facts(text: &str) -> Arc<morog_sema::FileFacts> {
        FileAnalysis::parse(
            Path::new("/vault/x.md"),
            text.into(),
            &LintConfig::default(),
            None,
            &morg_parser::TagTable::empty(),
        )
        .facts
    }

    const TOPIC: &str = "---\nid: sla\nkind: topic\nrelated: [lmr]\n---\n\nCited [@wright2004]. #anchor e1\n\nSee [lmr](id:lmr#def). #mine\n";
    const ARG: &str = "---\nid: arg\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: sla#e1, stance: supports }\n  - { to: lmr, stance: opposes }\n---\n";

    #[test]
    fn add_and_remove_are_inverse() {
        let topic = facts(TOPIC);
        let arg = facts(ARG);
        let mut xref = Xref::default();
        xref.add_file(FileId(0), &topic);
        xref.add_file(FileId(1), &arg);

        assert_eq!(xref.resolve("sla"), Some(FileId(0)));
        assert_eq!(xref.resolve("arg"), Some(FileId(1)));
        assert_eq!(xref.resolve("lmr"), None);
        assert!(xref.is_unresolved("lmr"));
        assert!(!xref.is_unresolved("sla"));

        // sla: one argues ref from arg. lmr: related + mention from topic,
        // argues from arg. wright2004: one cite from topic.
        assert_eq!(xref.refs_to("sla").count(), 1);
        assert_eq!(xref.refs_to("lmr").count(), 3);
        assert_eq!(xref.refs_to("wright2004").count(), 1);
        let cite = xref.refs_to("wright2004").next().unwrap();
        assert_eq!(cite.kind, RelationKind::Cites);
        assert_eq!(cite.file, FileId(0));
        let argues = xref.refs_to("sla").next().unwrap();
        assert!(matches!(argues.kind, RelationKind::Argues { .. }));
        assert_eq!(argues.target_anchor.as_deref(), Some("e1"));

        xref.remove_file(FileId(1), &arg);
        assert_eq!(xref.resolve("arg"), None);
        assert_eq!(xref.refs_to("sla").count(), 0);
        assert_eq!(xref.refs_to("lmr").count(), 2);

        xref.remove_file(FileId(0), &topic);
        assert!(xref.by_id.is_empty());
        assert!(xref.backrefs.is_empty());
    }

    #[test]
    fn duplicate_ids_keep_first_declarer() {
        let a = facts("---\nid: dup\nkind: topic\n---\n");
        let b = facts("---\nid: dup\nkind: topic\n---\n");
        let mut xref = Xref::default();
        xref.add_file(FileId(0), &a);
        xref.add_file(FileId(1), &b);
        assert_eq!(xref.by_id.get("dup").unwrap().len(), 2);
        assert_eq!(xref.resolve("dup"), Some(FileId(0)));

        xref.remove_file(FileId(0), &a);
        assert_eq!(xref.resolve("dup"), Some(FileId(1)));
    }

    #[test]
    fn snapshot_clone_shares_structure() {
        let topic = facts(TOPIC);
        let mut xref = Xref::default();
        xref.add_file(FileId(0), &topic);
        let snapshot = xref.clone();
        let arg = facts(ARG);
        xref.add_file(FileId(1), &arg);
        // The clone is unaffected by later writes.
        assert_eq!(snapshot.resolve("arg"), None);
        assert_eq!(xref.resolve("arg"), Some(FileId(1)));
    }
}

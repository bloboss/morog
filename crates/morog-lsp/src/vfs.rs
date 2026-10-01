//! File interning and editor overlays (plan §5.1).
//!
//! The [`Vfs`] owns the mapping between vault paths and dense [`FileId`]s,
//! plus the **overlays**: the text of documents currently open in the
//! editor, with their protocol version. Open buffers always win over
//! whatever the disk or the index says (plan §5.3), so everything else in
//! the server resolves file content through here.
//!
//! The vfs is deliberately protocol-free: it stores text and versions and
//! never sees LSP types; edit application happens in `from_proto` land.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Dense handle for one vault file (plan §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// An open document's editor state.
#[derive(Debug, Clone)]
pub struct Overlay {
    /// The client's document version, echoed to detect staleness.
    pub version: i32,
    pub text: Arc<str>,
}

/// Path interning plus open-document overlays.
#[derive(Debug, Default)]
pub struct Vfs {
    paths: Vec<Arc<Path>>,
    by_path: HashMap<Arc<Path>, FileId>,
    overlays: HashMap<FileId, Overlay>,
}

impl Vfs {
    /// Intern a path, allocating a [`FileId`] on first sight.
    ///
    /// Paths are interned as given (absolute, from the URI or the walk);
    /// callers normalize before interning.
    pub fn intern(&mut self, path: &Path) -> FileId {
        if let Some(&id) = self.by_path.get(path) {
            return id;
        }
        let id = FileId(u32::try_from(self.paths.len()).expect("more than u32::MAX files"));
        let arc: Arc<Path> = Arc::from(PathBuf::from(path));
        self.paths.push(Arc::clone(&arc));
        self.by_path.insert(arc, id);
        id
    }

    /// The id of an already-interned path.
    pub fn get(&self, path: &Path) -> Option<FileId> {
        self.by_path.get(path).copied()
    }

    /// The path behind an id.
    pub fn path(&self, id: FileId) -> &Arc<Path> {
        &self.paths[id.0 as usize]
    }

    /// Record an opened document's text (didOpen).
    pub fn open(&mut self, id: FileId, version: i32, text: Arc<str>) {
        self.overlays.insert(id, Overlay { version, text });
    }

    /// Replace an open document's text (didChange, after edit application).
    pub fn change(&mut self, id: FileId, version: i32, text: Arc<str>) {
        self.overlays.insert(id, Overlay { version, text });
    }

    /// Drop the overlay (didClose); the file falls back to disk + index.
    pub fn close(&mut self, id: FileId) {
        self.overlays.remove(&id);
    }

    /// The overlay for an open document, if any.
    pub fn overlay(&self, id: FileId) -> Option<&Overlay> {
        self.overlays.get(&id)
    }

    pub fn is_open(&self, id: FileId) -> bool {
        self.overlays.contains_key(&id)
    }

    /// Ids of all open documents.
    pub fn open_files(&self) -> impl Iterator<Item = FileId> + '_ {
        self.overlays.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_stable_and_distinct() {
        let mut vfs = Vfs::default();
        let a = vfs.intern(Path::new("/vault/a.md"));
        let b = vfs.intern(Path::new("/vault/b.md"));
        assert_ne!(a, b);
        assert_eq!(vfs.intern(Path::new("/vault/a.md")), a);
        assert_eq!(vfs.get(Path::new("/vault/b.md")), Some(b));
        assert_eq!(vfs.get(Path::new("/vault/c.md")), None);
        assert_eq!(vfs.path(a).as_ref(), Path::new("/vault/a.md"));
    }

    #[test]
    fn overlay_lifecycle() {
        let mut vfs = Vfs::default();
        let id = vfs.intern(Path::new("/vault/a.md"));
        assert!(!vfs.is_open(id));

        vfs.open(id, 1, "one".into());
        assert_eq!(vfs.overlay(id).unwrap().version, 1);
        assert_eq!(&*vfs.overlay(id).unwrap().text, "one");

        vfs.change(id, 2, "two".into());
        assert_eq!(vfs.overlay(id).unwrap().version, 2);
        assert_eq!(&*vfs.overlay(id).unwrap().text, "two");
        assert_eq!(vfs.open_files().collect::<Vec<_>>(), vec![id]);

        vfs.close(id);
        assert!(vfs.overlay(id).is_none());
    }
}

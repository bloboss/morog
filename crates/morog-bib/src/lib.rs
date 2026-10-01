//! Bibliography access for the morog research toolkit.
//!
//! Source notes and inline `[@key]` citations resolve against a
//! bibliography provider behind the [`BibStore`] trait. The real provider
//! is [`BibratStore`] (plan §4), a read-only adapter over a bibrat data
//! directory; [`FixtureStore`] serves tests and development from an
//! in-memory map.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

mod bibrat;

pub use bibrat::{BibratStore, resolve_data_dir};

/// A citation key as it appears in notes, e.g. `shabanovSTA`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CiteKey(pub String);

impl CiteKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

/// Where an entry lives inside the bibliography store.
///
/// For the v1 bibrat adapter this is a single flat category name (e.g.
/// `plastix`); nested collection paths are **deferred** until the bibrat
/// TOML schema defines their semantics (plan §4.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CollectionPath(pub String);

impl std::fmt::Display for CollectionPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Metadata extracted from an entry's `citation.bib`.
///
/// bibrat stores BibTeX today; this moves to the bibrat TOML schema when it
/// lands (plan §4.1 B2). Every field is optional — a `.bib` without a year
/// is data, not an error.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BibMeta {
    pub title: Option<String>,
    pub year: Option<i32>,
    pub doi: Option<String>,
}

/// A resolved bibliography entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BibEntry {
    /// The canonical citation key. For bibrat this is the **directory
    /// name** (bibrat names entry directories by key); a differing citekey
    /// inside `citation.bib` is reported in [`warnings`](Self::warnings).
    pub key: CiteKey,
    /// The collection holding the canonical entry directory.
    pub collection: CollectionPath,
    /// Extra collections that reference this entry (plan §4.1 B3).
    pub also_in: Vec<CollectionPath>,
    /// The entry directory on disk (empty for fixture entries).
    pub dir: PathBuf,
    /// Parsed metadata; `None` when `citation.bib` is missing or malformed
    /// (the cause lands in [`warnings`](Self::warnings), never a walk
    /// failure).
    pub meta: Option<BibMeta>,
    /// Entry tags from a per-entry `TAGS.yml`, names only.
    pub tags: Vec<String>,
    /// Non-fatal problems found while reading this entry.
    pub warnings: Vec<String>,
}

impl BibEntry {
    /// A minimal entry with only a key — fixture and test construction.
    pub fn minimal(key: impl Into<String>) -> Self {
        Self {
            key: CiteKey::new(key),
            collection: CollectionPath(String::new()),
            also_in: Vec::new(),
            dir: PathBuf::new(),
            meta: None,
            tags: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Error surface for bibliography discovery and lookup.
#[derive(Debug)]
pub struct BibError(pub String);

impl std::fmt::Display for BibError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BibError {}

/// A source of bibliography entries.
pub trait BibStore {
    /// Locate and load bibliography data for the given vault root.
    fn discover(&mut self, vault_root: &Path) -> Result<(), BibError>;

    /// All entries currently known to the store.
    fn entries(&self) -> Vec<&BibEntry>;

    /// Resolve one citation key to its entry, if known.
    fn resolve(&self, key: &CiteKey) -> Option<&BibEntry>;

    /// The store's tag vocabulary (root `TAGS.yml` for bibrat), names only.
    ///
    /// Defaults to empty for providers without a tag concept.
    fn tags(&self) -> Vec<String> {
        Vec::new()
    }
}

/// An in-memory [`BibStore`] for tests and development.
#[derive(Debug, Default)]
pub struct FixtureStore {
    entries: HashMap<CiteKey, BibEntry>,
}

impl FixtureStore {
    pub fn new(entries: HashMap<CiteKey, BibEntry>) -> Self {
        Self { entries }
    }

    /// Build a store of [minimal](BibEntry::minimal) entries from keys.
    pub fn from_keys<I, S>(keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::new(
            keys.into_iter()
                .map(|k| {
                    let entry = BibEntry::minimal(k);
                    (entry.key.clone(), entry)
                })
                .collect(),
        )
    }
}

impl BibStore for FixtureStore {
    fn discover(&mut self, _vault_root: &Path) -> Result<(), BibError> {
        // Fixtures are supplied up front; nothing to discover.
        Ok(())
    }

    fn entries(&self) -> Vec<&BibEntry> {
        self.entries.values().collect()
    }

    fn resolve(&self, key: &CiteKey) -> Option<&BibEntry> {
        self.entries.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_store_resolves_known_keys() {
        let key = CiteKey::new("shabanovSTA");
        let mut store = FixtureStore::from_keys(["shabanovSTA"]);

        assert!(store.discover(Path::new("/nonexistent")).is_ok());
        assert_eq!(store.entries().len(), 1);
        assert!(store.resolve(&key).is_some());
        assert!(store.resolve(&CiteKey::new("missing")).is_none());
        assert!(store.tags().is_empty());
    }
}

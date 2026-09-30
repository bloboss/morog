//! Bibliography access for the morog research toolkit.
//!
//! Source notes and inline `[@key]` citations resolve against a
//! bibliography provider behind the [`BibStore`] trait. The planned
//! provider is **bibrat**, but its adapter is blocked on a pending TOML
//! schema; until then [`FixtureStore`] serves tests and development from an
//! in-memory map.

use std::collections::HashMap;
use std::path::Path;

/// A citation key as it appears in notes, e.g. `shabanovSTA`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CiteKey(pub String);

impl CiteKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

/// A resolved bibliography entry.
///
/// Placeholder shape; the real field set follows the bibrat TOML schema
/// once it settles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BibEntry {
    pub key: CiteKey,
    pub title: Option<String>,
}

/// Error surface for bibliography discovery and lookup.
///
/// Placeholder: grows real variants with the bibrat adapter.
#[derive(Debug)]
pub struct BibError(pub String);

/// A source of bibliography entries.
pub trait BibStore {
    /// Locate and load bibliography data for the given vault root.
    fn discover(&mut self, vault_root: &Path) -> Result<(), BibError>;

    /// All entries currently known to the store.
    fn entries(&self) -> Vec<&BibEntry>;

    /// Resolve one citation key to its entry, if known.
    fn resolve(&self, key: &CiteKey) -> Option<&BibEntry>;
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
        let entry = BibEntry {
            key: key.clone(),
            title: Some("Source-Topic-Argument".into()),
        };
        let mut store = FixtureStore::new(HashMap::from([(key.clone(), entry)]));

        assert!(store.discover(Path::new("/nonexistent")).is_ok());
        assert_eq!(store.entries().len(), 1);
        assert!(store.resolve(&key).is_some());
        assert!(store.resolve(&CiteKey::new("missing")).is_none());
    }
}

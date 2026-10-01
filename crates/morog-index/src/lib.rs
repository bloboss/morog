//! Disposable SQLite index for the morog research toolkit (plan §3).
//!
//! The index is a **cache, never a source of truth**: it is rebuilt from
//! read-only access to the vault and can be deleted at any time. It stores
//! the `FileFacts` extracted by `morog-sema` (notes, relations, citations,
//! block anchors) and answers the queries behind `morog query`,
//! `morog research`, and `morog graph`.

use std::path::{Path, PathBuf};

pub mod pipeline;
pub mod query;
pub mod schema;
pub mod store;

pub use pipeline::{IndexStats, index_vault};
pub use query::{QueryOutput, Value, run_query};
pub use schema::SCHEMA_VERSION;
pub use store::{delete_file_rows, load_file_facts, store_file_facts};

/// Everything that can go wrong while building or querying the index.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Sql(rusqlite::Error),
    /// The database carries a different `user_version` (read-only open only;
    /// writers rebuild instead — plan §3.1 principle 4).
    SchemaVersion {
        found: i64,
        expected: i64,
    },
    /// `morog query` was given something other than a single SELECT.
    NotSelect,
    /// No index database exists yet for this vault.
    MissingIndex(PathBuf),
    /// Stored rows that cannot be mapped back to facts.
    Corrupt(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io error: {e}"),
            Self::Sql(e) => write!(f, "sqlite error: {e}"),
            Self::SchemaVersion { found, expected } => write!(
                f,
                "index schema version {found} != {expected}; run `morog index` to rebuild"
            ),
            Self::NotSelect => write!(f, "only a single SELECT statement is allowed"),
            Self::MissingIndex(path) => {
                write!(f, "no index at {}; run `morog index` first", path.display())
            }
            Self::Corrupt(msg) => write!(f, "corrupt index: {msg}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Sql(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

/// The cache database path for a vault:
/// `$XDG_CACHE_HOME/morog/<vault-hash>/index.sqlite`.
///
/// Falls back to `$HOME/.cache` when `XDG_CACHE_HOME` is unset, per the XDG
/// Base Directory spec. The vault hash is a stable blake3 digest of the
/// vault path, so distinct vaults get distinct caches.
pub fn cache_db_path(vault_root: &Path) -> PathBuf {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".cache")
        });
    cache_db_path_in(&cache_home, vault_root)
}

fn cache_db_path_in(cache_home: &Path, vault_root: &Path) -> PathBuf {
    let hex = blake3::hash(vault_root.as_os_str().as_encoded_bytes()).to_hex();
    let vault_hash = &hex.as_str()[..16];
    cache_home
        .join("morog")
        .join(vault_hash)
        .join("index.sqlite")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_is_stable_and_vault_specific() {
        let cache = Path::new("/cache");
        let a = cache_db_path_in(cache, Path::new("/vaults/research"));
        let b = cache_db_path_in(cache, Path::new("/vaults/research"));
        let c = cache_db_path_in(cache, Path::new("/vaults/other"));

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("/cache/morog"));
        assert!(a.ends_with("index.sqlite"));
    }
}

//! Disposable SQLite index for the morog research toolkit.
//!
//! The index is a **cache, never a source of truth**: it is rebuilt
//! asynchronously from read-only access to the vault and can be deleted at
//! any time. It stores the `FileFacts` extracted by `morog-sema` (notes,
//! relations, citations, block anchors) and answers the queries behind
//! `morog query`, `morog research`, and `morog graph`.
//!
//! `rusqlite` is declared in the workspace dependencies but not wired in
//! yet; only cache-path resolution is implemented.

use std::path::{Path, PathBuf};

pub mod schema {
    //! SQLite schema and migrations (not yet implemented).
}

pub mod build {
    //! Asynchronous index (re)build from a read-only vault walk
    //! (not yet implemented).
}

pub mod query {
    //! Typed read queries over the index (not yet implemented).
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

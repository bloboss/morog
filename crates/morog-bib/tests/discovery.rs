//! Discovery-order tests for the bibrat data directory (plan §4.1 B4):
//! config `[store] data_dir` > `$BIBRAT_DATA_DIR` > `$XDG_DATA_HOME/bibrat`
//! > `~/.local/share/bibrat`.
//!
//! These tests mutate process environment variables, so they serialize on a
//! single mutex and every test sets *all* the variables discovery reads.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use morog_bib::{BibStore, BibratStore, resolve_data_dir};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Deterministic environment for one discovery test. `None` removes.
fn set_env(vars: &[(&str, Option<&Path>)]) {
    for (key, value) in vars {
        // SAFETY: all tests touching the environment hold ENV_LOCK, and
        // this test binary spawns no threads of its own.
        unsafe {
            match value {
                Some(path) => std::env::set_var(key, path),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "morog-bib-discovery-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A minimal valid store: one category, one entry.
fn make_store(root: &Path, key: &str) {
    let entry = root.join("cat").join(key);
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::write(
        entry.join("citation.bib"),
        format!("@misc{{{key}, title = {{Discovered}}, year = {{2024}}}}\n"),
    )
    .unwrap();
}

#[test]
fn config_data_dir_wins_over_everything() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new("config");
    let config_store = tmp.path().join("from-config");
    let env_store = tmp.path().join("from-env");
    make_store(&config_store, "via_config");
    make_store(&env_store, "via_env");
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!("[store]\ndata_dir = \"{}\"\n", config_store.display()),
    )
    .unwrap();

    set_env(&[
        ("BIBRAT_CONFIG", Some(&config)),
        ("BIBRAT_DATA_DIR", Some(&env_store)),
        ("XDG_DATA_HOME", Some(tmp.path())),
        ("XDG_CONFIG_HOME", Some(tmp.path())),
        ("HOME", Some(tmp.path())),
    ]);
    assert_eq!(resolve_data_dir().as_deref(), Some(config_store.as_path()));
    let store = BibratStore::discover_default().unwrap().unwrap();
    assert_eq!(store.root(), config_store);
    assert_eq!(store.entries()[0].key.0, "via_config");
}

#[test]
fn env_data_dir_beats_xdg_and_home() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new("envvar");
    let env_store = tmp.path().join("from-env");
    make_store(&env_store, "via_env");
    make_store(&tmp.path().join("xdg").join("bibrat"), "via_xdg");
    let no_config = tmp.path().join("missing-config.toml");

    set_env(&[
        ("BIBRAT_CONFIG", Some(&no_config)),
        ("BIBRAT_DATA_DIR", Some(&env_store)),
        ("XDG_DATA_HOME", Some(&tmp.path().join("xdg"))),
        ("XDG_CONFIG_HOME", Some(tmp.path())),
        ("HOME", Some(tmp.path())),
    ]);
    let store = BibratStore::discover_default().unwrap().unwrap();
    assert_eq!(store.root(), env_store);
    assert_eq!(store.entries()[0].key.0, "via_env");
}

#[test]
fn xdg_data_home_beats_home_fallback() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new("xdg");
    let xdg = tmp.path().join("xdg");
    make_store(&xdg.join("bibrat"), "via_xdg");
    make_store(
        &tmp.path().join(".local").join("share").join("bibrat"),
        "via_home",
    );
    let no_config = tmp.path().join("missing-config.toml");

    set_env(&[
        ("BIBRAT_CONFIG", Some(&no_config)),
        ("BIBRAT_DATA_DIR", None),
        ("XDG_DATA_HOME", Some(&xdg)),
        ("XDG_CONFIG_HOME", Some(tmp.path())),
        ("HOME", Some(tmp.path())),
    ]);
    assert_eq!(
        resolve_data_dir().as_deref(),
        Some(xdg.join("bibrat").as_path())
    );
    let store = BibratStore::discover_default().unwrap().unwrap();
    assert_eq!(store.entries()[0].key.0, "via_xdg");
}

#[test]
fn home_fallback_and_missing_store_is_none() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new("home");
    let no_config = tmp.path().join("missing-config.toml");
    set_env(&[
        ("BIBRAT_CONFIG", Some(&no_config)),
        ("BIBRAT_DATA_DIR", None),
        ("XDG_DATA_HOME", None),
        ("XDG_CONFIG_HOME", Some(tmp.path())),
        ("HOME", Some(tmp.path())),
    ]);

    // Resolution still names ~/.local/share/bibrat …
    let expected = tmp.path().join(".local").join("share").join("bibrat");
    assert_eq!(resolve_data_dir().as_deref(), Some(expected.as_path()));
    // … but with nothing on disk, discovery is cleanly "no store".
    assert!(BibratStore::discover_default().unwrap().is_none());

    make_store(&expected, "via_home");
    let store = BibratStore::discover_default().unwrap().unwrap();
    assert_eq!(store.entries()[0].key.0, "via_home");
}

#[test]
fn config_default_category_is_skipped_as_staging() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new("staging");
    let root = tmp.path().join("store");
    make_store(&root, "kept");
    make_store(&root, "dropped"); // same category, second entry
    // Move `dropped` into a custom staging category named by the config.
    std::fs::create_dir_all(root.join("drafts")).unwrap();
    std::fs::rename(
        root.join("cat").join("dropped"),
        root.join("drafts").join("dropped"),
    )
    .unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[store]\ndata_dir = \"{}\"\ndefault_category = \"drafts\"\n",
            root.display()
        ),
    )
    .unwrap();

    set_env(&[
        ("BIBRAT_CONFIG", Some(&config)),
        ("BIBRAT_DATA_DIR", None),
        ("XDG_DATA_HOME", None),
        ("XDG_CONFIG_HOME", Some(tmp.path())),
        ("HOME", Some(tmp.path())),
    ]);
    let store = BibratStore::discover_default().unwrap().unwrap();
    let keys: Vec<&str> = store.entries().iter().map(|e| e.key.0.as_str()).collect();
    assert_eq!(keys, ["kept"]);
}

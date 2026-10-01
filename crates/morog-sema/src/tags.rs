//! User-defined tag configuration (plan §10 T1): the `[tags]` section and
//! its resolution into a [`TagTable`].
//!
//! Declarations are resolved across three layers, highest precedence first:
//!
//! 1. the vault-local `.morog.toml` at the vault root — the portable home
//!    for definitions that must travel with a shared vault;
//! 2. morog's user config, `$XDG_CONFIG_HOME/morog/config.toml`;
//! 3. **fallback**: morg's own config, `$XDG_CONFIG_HOME/morg/config.toml` —
//!    single-source `[tags]` definitions written for morg are honored by
//!    morog, so a tag declared once works in both toolchains.
//!
//! Merging is key-by-key like the other config keys: a tag name declared at
//! a higher layer wins **wholesale** (its whole declaration replaces the
//! lower one). Invalid declarations are warned about and dropped
//! individually; the rest of the table still builds. This module lives in
//! `morog-sema` so both the CLI (`crates/morog/src/config.rs`) and the LSP
//! (which resolves the same files from the workspace folder) share one
//! implementation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use morg_parser::{CustomArgKind, TagDeclaration, TagTable};
use serde::Deserialize;

/// Name of the vault-local override file at the vault root (the same file
/// `crates/morog/src/config.rs` reads for `[publish]`/`[lint]`).
pub const VAULT_TAGS_FILE: &str = ".morog.toml";

/// One `[tags.<name>]` entry as written in config. Exactly one of
/// `pattern`/`kind` must be set; [`build_tag_table`] enforces that per
/// declaration (warn + drop, never a hard error).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct TagDecl {
    /// Regex with named capture groups, matched against the raw argument.
    pub pattern: Option<String>,
    /// Built-in argument parser shorthand
    /// (`duration` | `date` | `timestamp` | `slug`).
    pub kind: Option<String>,
}

/// Tag name → declaration. A `BTreeMap` keeps warnings and downstream
/// fingerprints deterministic.
pub type TagsMap = BTreeMap<String, TagDecl>;

/// The `[tags]` section of any config file; every other section is ignored,
/// so the same reader works for `.morog.toml`, morog's `config.toml`, and
/// morg's `config.toml` alike.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct TagsFile {
    tags: TagsMap,
}

/// Read the `[tags]` section of one TOML file. `Ok(None)` when the file
/// does not exist; a malformed file is an error carrying its path.
pub fn read_tags_file(path: &Path) -> Result<Option<TagsMap>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let file: TagsFile =
        toml::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))?;
    Ok(Some(file.tags))
}

/// `$XDG_CONFIG_HOME`, falling back to `$HOME/.config` per the XDG Base
/// Directory spec.
fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
}

/// morog's user config: `$XDG_CONFIG_HOME/morog/config.toml`.
pub fn morog_config_path() -> Option<PathBuf> {
    Some(config_home()?.join("morog").join("config.toml"))
}

/// morg's own config: `$XDG_CONFIG_HOME/morg/config.toml` — the `[tags]`
/// fallback layer.
pub fn morg_config_path() -> Option<PathBuf> {
    Some(config_home()?.join("morg").join("config.toml"))
}

/// The config files whose `[tags]` sections feed the table, lowest
/// precedence first: morg config, morog config, vault `.morog.toml`. Also
/// the watch list for live reconfiguration (LSP).
pub fn tag_config_paths(vault_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(3);
    paths.extend(morg_config_path());
    paths.extend(morog_config_path());
    paths.push(vault_root.join(VAULT_TAGS_FILE));
    paths
}

/// Resolve the `[tags]` declarations for a vault across the three layers.
/// Unreadable or malformed files degrade to warnings — a broken config must
/// never make notes unreadable (plan §10.1 principle 1).
pub fn resolve_tags(vault_root: &Path) -> (TagsMap, Vec<String>) {
    let mut merged = TagsMap::new();
    let mut warnings = Vec::new();
    for path in tag_config_paths(vault_root) {
        match read_tags_file(&path) {
            Ok(Some(tags)) => merged.extend(tags),
            Ok(None) => {}
            Err(warning) => warnings.push(warning),
        }
    }
    (merged, warnings)
}

/// Compile a merged map into a [`TagTable`], dropping invalid declarations
/// individually with a warning each and keeping the rest.
pub fn build_tag_table(map: &TagsMap) -> (TagTable, Vec<String>) {
    let mut warnings = Vec::new();
    let mut declarations = Vec::with_capacity(map.len());
    for (name, decl) in map {
        let kind = match decl.kind.as_deref() {
            Some(word) => match CustomArgKind::from_str(word) {
                Some(kind) => Some(kind),
                None => {
                    warnings.push(format!(
                        "tag '{name}': unknown kind '{word}' \
                         (expected duration, date, timestamp, or slug); declaration dropped"
                    ));
                    continue;
                }
            },
            None => None,
        };
        let declaration = TagDeclaration {
            name: name.clone(),
            pattern: decl.pattern.clone(),
            kind,
        };
        // Validate one declaration at a time so a single bad entry cannot
        // take the whole table down (TagTable::build aborts on first error).
        match TagTable::build([declaration.clone()]) {
            Ok(_) => declarations.push(declaration),
            Err(e) => warnings.push(format!("{e}; declaration dropped")),
        }
    }
    let table = TagTable::build(declarations).unwrap_or_else(|e| {
        // Unreachable: every declaration validated alone, and names are
        // unique by construction (BTreeMap keys).
        warnings.push(format!("{e}; tag table dropped"));
        TagTable::empty()
    });
    (table, warnings)
}

/// Resolve and compile the tag table for a vault in one step — the LSP's
/// startup (and config-watch) entry point. The CLI goes through
/// `Config::tag_table` instead, which merges `[tags]` alongside the other
/// config keys.
pub fn load_tag_table(vault_root: &Path) -> (TagTable, Vec<String>) {
    let (map, mut warnings) = resolve_tags(vault_root);
    let (table, mut build_warnings) = build_tag_table(&map);
    warnings.append(&mut build_warnings);
    (table, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(pattern: Option<&str>, kind: Option<&str>) -> TagDecl {
        TagDecl {
            pattern: pattern.map(str::to_owned),
            kind: kind.map(str::to_owned),
        }
    }

    #[test]
    fn build_compiles_patterns_and_kinds() {
        let map = TagsMap::from([
            (
                "book".to_owned(),
                decl(Some(r#""(?<title>[^"]+)"\s+by\s+(?<author>.+)"#), None),
            ),
            ("reading-time".to_owned(), decl(None, Some("duration"))),
        ]);
        let (table, warnings) = build_tag_table(&map);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.group_names("book").unwrap(),
            vec!["title".to_owned(), "author".to_owned()]
        );
        assert_eq!(
            table.group_names("reading-time").unwrap(),
            vec!["duration".to_owned()]
        );
    }

    #[test]
    fn invalid_declarations_warn_and_drop_individually() {
        let map = TagsMap::from([
            // Bad regex.
            ("broken".to_owned(), decl(Some("(?<x>["), None)),
            // Unknown kind word.
            ("odd".to_owned(), decl(None, Some("frobnicate"))),
            // Built-in collision.
            ("deadline".to_owned(), decl(None, Some("date"))),
            // pattern and kind are mutually exclusive.
            ("both".to_owned(), decl(Some(".*"), Some("slug"))),
            // Neither rule.
            ("neither".to_owned(), decl(None, None)),
            // The one good declaration survives.
            ("good".to_owned(), decl(None, Some("slug"))),
        ]);
        let (table, warnings) = build_tag_table(&map);
        assert_eq!(table.len(), 1, "{warnings:?}");
        assert!(table.contains("good"));
        assert_eq!(warnings.len(), 5, "{warnings:?}");
        for needle in ["broken", "odd", "deadline", "both", "neither"] {
            assert!(
                warnings.iter().any(|w| w.contains(needle)),
                "no warning names {needle}: {warnings:?}"
            );
        }
    }

    #[test]
    fn higher_layers_win_wholesale() {
        // `extend` semantics: an upper declaration replaces the lower one
        // entirely (pattern does not leak under a kind override).
        let mut merged = TagsMap::from([
            ("book".to_owned(), decl(Some(".*"), None)),
            ("kept".to_owned(), decl(None, Some("slug"))),
        ]);
        merged.extend(TagsMap::from([(
            "book".to_owned(),
            decl(None, Some("slug")),
        )]));
        assert_eq!(merged["book"], decl(None, Some("slug")));
        assert_eq!(merged["kept"], decl(None, Some("slug")));
    }

    #[test]
    fn read_tags_file_reports_missing_and_malformed() {
        let dir = std::env::temp_dir().join(format!(
            "morog-sema-tags-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        assert_eq!(read_tags_file(&dir.join("absent.toml")).unwrap(), None);

        let good = dir.join("good.toml");
        std::fs::write(
            &good,
            "[publish]\nbase_iri = \"https://x\"\n[tags.book]\npattern = \".*\"\n",
        )
        .unwrap();
        let tags = read_tags_file(&good).unwrap().unwrap();
        assert_eq!(tags["book"], decl(Some(".*"), None));

        let bad = dir.join("bad.toml");
        std::fs::write(&bad, "[tags\n").unwrap();
        let err = read_tags_file(&bad).unwrap_err();
        assert!(err.contains("invalid config"), "{err}");
        assert!(err.contains("bad.toml"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tag_config_paths_order_is_morg_then_morog_then_vault() {
        let paths = tag_config_paths(Path::new("/vault"));
        let rendered: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(rendered.last().unwrap(), "/vault/.morog.toml");
        // The XDG layers precede the vault file (lowest precedence first).
        assert!(
            rendered[..rendered.len() - 1]
                .iter()
                .any(|p| p.ends_with("morg/config.toml"))
        );
        assert!(
            rendered[..rendered.len() - 1]
                .iter()
                .any(|p| p.ends_with("morog/config.toml"))
        );
    }
}

//! The v1 bibrat adapter (plan §4, task M3).
//!
//! Read-only view over a bibrat data directory in the shape **observed in
//! the live store** (plan §4.1, 2026-09-30):
//!
//! - flat top-level **category** directories (`plastix`, `test`, …);
//!   staging directories (`inbox/`, `parsing/`, the config's
//!   `default_category`) and dot-entries (`.active-category`, `.git`) are
//!   skipped;
//! - an **entry** is a directory containing `citation.bib` (BibTeX), the
//!   marker and only metadata file today. The **directory name is the
//!   canonical citation key**; a differing citekey inside the `.bib` is an
//!   entry warning. Entry internals (`Snapshot/`, `notes/`, attachments)
//!   are ignored;
//! - a plain **reference file** inside a category, named like an entry and
//!   containing the canonical entry's path relative to the data root,
//!   records an extra collection membership (plan §4.1 B3);
//! - the root `TAGS.yml` is the tag vocabulary; a per-entry `TAGS.yml`
//!   carries entry tags.
//!
//! **Deferred — nested collections.** The live store nests nothing, and
//! the bibrat TOML schema that will define collection semantics is still
//! pending. A directory with a `citation.bib` is an entry wherever it is
//! found one level below a category; a category subdirectory *without* one
//! is skipped with a store warning rather than walked deeper.
//!
//! Metadata parsing uses the maintained `biblatex` crate (typst). A
//! malformed `citation.bib` yields `meta: None` plus an entry warning,
//! never a walk failure.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use biblatex::{Bibliography, ChunksExt, DateValue, PermissiveType};

use crate::{BibEntry, BibError, BibMeta, BibStore, CiteKey, CollectionPath};

/// The entry marker and metadata file (plan §4.1 B1, observed shape).
const MARKER: &str = "citation.bib";
/// Tag file name, at the root (vocabulary) and inside entries (entry tags).
const TAGS_FILE: &str = "TAGS.yml";
/// Staging directories always skipped, independent of configuration.
const STAGING: [&str; 2] = ["inbox", "parsing"];

/// Read-only [`BibStore`] over a bibrat data directory.
#[derive(Debug, Default)]
pub struct BibratStore {
    root: PathBuf,
    /// Extra staging directory names from the config (`default_category`).
    staging: Vec<String>,
    /// Entries by canonical key; `BTreeMap` for deterministic iteration.
    entries: BTreeMap<String, BibEntry>,
    /// Root `TAGS.yml` vocabulary, names only.
    tags: Vec<String>,
    /// Store-level problems (dangling references, skipped nesting, …).
    warnings: Vec<String>,
}

impl BibratStore {
    /// Open the store rooted at an explicit data directory (`--bib-root`).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, BibError> {
        let root: PathBuf = root.into();
        if !root.is_dir() {
            return Err(BibError(format!(
                "bibrat data directory {} does not exist",
                root.display()
            )));
        }
        let mut store = Self {
            root,
            staging: Vec::new(),
            ..Self::default()
        };
        store.load()?;
        Ok(store)
    }

    /// Discover the store per bibrat's resolution rules.
    ///
    /// The data directory is the config's `[store] data_dir` when set,
    /// else `$BIBRAT_DATA_DIR`, else `$XDG_DATA_HOME/bibrat`, else
    /// `~/.local/share/bibrat`. Returns `Ok(None)` when the resolved
    /// directory does not exist — morog then simply runs without a
    /// bibliography.
    pub fn discover_default() -> Result<Option<Self>, BibError> {
        let (config, mut warnings) = BibratConfig::load();
        let Some(root) = resolve_data_dir_with(&config) else {
            return Ok(None);
        };
        if !root.is_dir() {
            return Ok(None);
        }
        let mut store = Self {
            root,
            staging: config.default_category.into_iter().collect(),
            ..Self::default()
        };
        store.load()?;
        warnings.append(&mut store.warnings);
        store.warnings = warnings;
        Ok(Some(store))
    }

    /// The data directory this store reads from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Store-level, non-fatal problems found by the walk.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Walk the data directory and (re)build the entry map.
    fn load(&mut self) -> Result<(), BibError> {
        self.entries.clear();
        self.tags.clear();

        let root_tags = self.root.join(TAGS_FILE);
        if root_tags.is_file() {
            match std::fs::read_to_string(&root_tags) {
                Ok(src) => self.tags = parse_tags_yml(&src),
                Err(e) => self.warn(format!("cannot read {}: {e}", root_tags.display())),
            }
        }

        // Reference files found while walking: (category, name, content).
        let mut refs: Vec<(String, String, String)> = Vec::new();

        for category in sorted_dir(&self.root, &mut self.warnings)? {
            let name = category.name;
            if !category.is_dir || name.starts_with('.') {
                continue; // root files (TAGS.yml, config.example.toml), dotfiles
            }
            if STAGING.contains(&name.as_str()) || self.staging.contains(&name) {
                continue;
            }
            self.load_category(&category.path, &name, &mut refs)?;
        }

        self.resolve_refs(refs);
        Ok(())
    }

    /// Walk one category directory: entry dirs and reference files.
    fn load_category(
        &mut self,
        dir: &Path,
        category: &str,
        refs: &mut Vec<(String, String, String)>,
    ) -> Result<(), BibError> {
        let mut warnings = std::mem::take(&mut self.warnings);
        for child in sorted_dir(dir, &mut warnings)? {
            if child.name.starts_with('.') {
                continue;
            }
            if child.is_dir {
                if child.path.join(MARKER).is_file() {
                    let entry = read_entry(&child.path, &child.name, category);
                    if let Some(prior) = self.entries.get(&child.name) {
                        warnings.push(format!(
                            "duplicate entry directory for key `{}`: {} kept, {} ignored \
                             (duplicates should be reference files)",
                            child.name,
                            prior.dir.display(),
                            child.path.display()
                        ));
                    } else {
                        self.entries.insert(child.name, entry);
                    }
                } else {
                    warnings.push(format!(
                        "skipping {}: directory without {MARKER} (nested collections \
                         are deferred until the bibrat TOML schema lands)",
                        child.path.display()
                    ));
                }
            } else if child.name != TAGS_FILE {
                // A plain file named like an entry: reference file (B3).
                match std::fs::read_to_string(&child.path) {
                    Ok(content) => refs.push((category.to_owned(), child.name, content)),
                    Err(e) => {
                        warnings.push(format!(
                            "cannot read reference file {}: {e}",
                            child.path.display()
                        ));
                    }
                }
            }
        }
        self.warnings = warnings;
        Ok(())
    }

    /// Resolve reference files to canonical entries and record `also_in`.
    fn resolve_refs(&mut self, refs: Vec<(String, String, String)>) {
        for (category, name, content) in refs {
            let target = content.lines().map(str::trim).find(|l| !l.is_empty());
            let Some(target) = target else {
                self.warn(format!("reference file {category}/{name} is empty"));
                continue;
            };
            let target_dir = self.root.join(target);
            let Some(entry) = self
                .entries
                .values_mut()
                .find(|e| e.dir == target_dir || e.dir.ends_with(target))
            else {
                self.warnings.push(format!(
                    "reference file {category}/{name} points at `{target}`, \
                     which is not a known entry"
                ));
                continue;
            };
            if entry.key.0 != name {
                entry.warnings.push(format!(
                    "referenced from {category} under the name `{name}`, \
                     which differs from its key"
                ));
            }
            entry.also_in.push(CollectionPath(category));
        }
        for entry in self.entries.values_mut() {
            entry.also_in.sort_by(|a, b| a.0.cmp(&b.0));
        }
    }

    fn warn(&mut self, message: String) {
        self.warnings.push(message);
    }
}

impl BibStore for BibratStore {
    /// Reload the store. The vault root is irrelevant for bibrat — the data
    /// directory was fixed by [`BibratStore::open`] / `discover_default`.
    fn discover(&mut self, _vault_root: &Path) -> Result<(), BibError> {
        self.load()
    }

    fn entries(&self) -> Vec<&BibEntry> {
        self.entries.values().collect()
    }

    fn resolve(&self, key: &CiteKey) -> Option<&BibEntry> {
        self.entries.get(&key.0)
    }

    fn tags(&self) -> Vec<String> {
        self.tags.clone()
    }
}

/// One directory child, pre-sorted for deterministic walks.
struct Child {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

/// List a directory's children sorted by name; unreadable children become
/// warnings instead of walk failures.
fn sorted_dir(dir: &Path, warnings: &mut Vec<String>) -> Result<Vec<Child>, BibError> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| BibError(format!("cannot read directory {}: {e}", dir.display())))?;
    let mut out = Vec::new();
    for entry in rd {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warnings.push(format!("while reading {}: {e}", dir.display()));
                continue;
            }
        };
        let Ok(name) = entry.file_name().into_string() else {
            warnings.push(format!(
                "skipping non-UTF-8 name in {}",
                entry.path().display()
            ));
            continue;
        };
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        out.push(Child {
            path: entry.path(),
            name,
            is_dir,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Build one entry from its directory; never fails (plan §4.1 tolerance).
fn read_entry(dir: &Path, key: &str, category: &str) -> BibEntry {
    let mut warnings = Vec::new();
    let marker = dir.join(MARKER);
    let meta = match std::fs::read_to_string(&marker) {
        Err(e) => {
            warnings.push(format!("cannot read {MARKER}: {e}"));
            None
        }
        Ok(src) => match Bibliography::parse(&src) {
            Err(e) => {
                warnings.push(format!("malformed {MARKER}: {e}"));
                None
            }
            Ok(bib) => match bib.iter().next() {
                None => {
                    warnings.push(format!("{MARKER} contains no entries"));
                    None
                }
                Some(entry) => {
                    if entry.key != key {
                        warnings.push(format!(
                            "citekey `{}` inside {MARKER} differs from the directory \
                             name `{key}` (the directory name is canonical)",
                            entry.key
                        ));
                    }
                    Some(extract_meta(entry))
                }
            },
        },
    };

    let mut tags = Vec::new();
    let tags_file = dir.join(TAGS_FILE);
    if tags_file.is_file() {
        match std::fs::read_to_string(&tags_file) {
            Ok(src) => tags = parse_tags_yml(&src),
            Err(e) => warnings.push(format!("cannot read {TAGS_FILE}: {e}")),
        }
    }

    BibEntry {
        key: CiteKey::new(key),
        collection: CollectionPath(category.to_owned()),
        also_in: Vec::new(),
        dir: dir.to_path_buf(),
        meta,
        tags,
        warnings,
    }
}

/// Pull title / year / doi out of a parsed BibTeX entry.
fn extract_meta(entry: &biblatex::Entry) -> BibMeta {
    let title = entry
        .title()
        .ok()
        .map(|chunks| chunks.format_verbatim())
        .filter(|t| !t.is_empty());
    let year = entry.date().ok().and_then(|date| match date {
        PermissiveType::Typed(date) => Some(match date.value {
            DateValue::At(dt)
            | DateValue::After(dt)
            | DateValue::Before(dt)
            | DateValue::Between(dt, _) => dt.year,
        }),
        PermissiveType::Chunks(_) => None,
    });
    let doi = entry.doi().ok().filter(|d| !d.is_empty());
    BibMeta { title, year, doi }
}

// ---------------------------------------------------------------------------
// Discovery: bibrat config and data-dir resolution
// ---------------------------------------------------------------------------

/// The slice of `bibrat/config.toml` morog cares about.
#[derive(Debug, Default)]
struct BibratConfig {
    /// `[store] data_dir` — overrides the environment chain when non-empty.
    data_dir: Option<PathBuf>,
    /// `[store] default_category` — a staging directory the walk skips.
    default_category: Option<String>,
}

impl BibratConfig {
    /// Read the config from `$BIBRAT_CONFIG`, else
    /// `$XDG_CONFIG_HOME/bibrat/config.toml`, else
    /// `~/.config/bibrat/config.toml`. A missing file is the default
    /// config; an unreadable or malformed one is a warning.
    fn load() -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let Some(path) = config_path() else {
            return (Self::default(), warnings);
        };
        let src = match std::fs::read_to_string(&path) {
            Ok(src) => src,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), warnings);
            }
            Err(e) => {
                warnings.push(format!("cannot read {}: {e}", path.display()));
                return (Self::default(), warnings);
            }
        };
        match src.parse::<toml::Table>() {
            Ok(table) => (Self::from_table(&table), warnings),
            Err(e) => {
                warnings.push(format!("malformed {}: {e}", path.display()));
                (Self::default(), warnings)
            }
        }
    }

    fn from_table(table: &toml::Table) -> Self {
        let store = table.get("store").and_then(|v| v.as_table());
        let str_key = |key: &str| {
            store
                .and_then(|s| s.get(key))
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
        };
        Self {
            data_dir: str_key("data_dir").map(expand_tilde),
            default_category: str_key("default_category").map(str::to_owned),
        }
    }
}

/// `$BIBRAT_CONFIG`, else `<config home>/bibrat/config.toml`.
fn config_path() -> Option<PathBuf> {
    if let Some(path) = env_path("BIBRAT_CONFIG") {
        return Some(path);
    }
    let config_home = env_path("XDG_CONFIG_HOME").or_else(|| Some(home_dir()?.join(".config")))?;
    Some(config_home.join("bibrat").join("config.toml"))
}

/// Resolve the bibrat data directory bibrat itself would use (plan §4.1):
/// config `[store] data_dir`, else `$BIBRAT_DATA_DIR`, else
/// `$XDG_DATA_HOME/bibrat`, else `~/.local/share/bibrat`. `None` only when
/// even `$HOME` is unset.
pub fn resolve_data_dir() -> Option<PathBuf> {
    resolve_data_dir_with(&BibratConfig::load().0)
}

fn resolve_data_dir_with(config: &BibratConfig) -> Option<PathBuf> {
    if let Some(dir) = &config.data_dir {
        return Some(dir.clone());
    }
    if let Some(dir) = env_path("BIBRAT_DATA_DIR") {
        return Some(dir);
    }
    if let Some(data_home) = env_path("XDG_DATA_HOME") {
        return Some(data_home.join("bibrat"));
    }
    Some(home_dir()?.join(".local").join("share").join("bibrat"))
}

/// A non-empty environment variable as a path.
fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    env_path("HOME")
}

/// Expand a leading `~/` against `$HOME`, like bibrat does.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

// ---------------------------------------------------------------------------
// TAGS.yml
// ---------------------------------------------------------------------------

/// Extract tag names from a bibrat `TAGS.yml`.
///
/// Deliberately minimal (plan §4.1 B6 wants names only): a top-level
/// `tags:` key followed by either indented `name: <anything>` map entries
/// (the root vocabulary) or `- name` list items (per-entry files). Anything
/// fancier waits for bibrat to define the format.
fn parse_tags_yml(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_tags = false;
    let mut tag_indent: Option<usize> = None;
    for line in src.lines() {
        let item = line.trim();
        if item.is_empty() || item.starts_with('#') {
            continue;
        }
        let top_level = !line.starts_with([' ', '\t', '-']);
        if top_level {
            in_tags = item == "tags:";
            tag_indent = None;
            continue;
        }
        if !in_tags {
            continue;
        }
        // Only the first indentation level holds tag names; deeper lines
        // are per-tag metadata.
        let indent = line.len() - line.trim_start().len();
        match tag_indent {
            None => tag_indent = Some(indent),
            Some(level) if indent > level => continue,
            Some(_) => {}
        }
        let name = match item.strip_prefix('-') {
            Some(rest) => rest.trim(),
            None => item.split(':').next().unwrap_or("").trim(),
        };
        let name = name.trim_matches(|c| c == '"' || c == '\'');
        if !name.is_empty() {
            out.push(name.to_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_yml_map_and_list_forms() {
        // Root vocabulary: a map with null/metadata values.
        let root = "# vocabulary\ntags:\n  plasticity: null\n  methods:\n    color: red\n";
        assert_eq!(parse_tags_yml(root), ["plasticity", "methods"]);

        // Per-entry file: a list, items at column zero.
        let entry = "tags:\n- test-tag\n- 'quoted tag'\n";
        assert_eq!(parse_tags_yml(entry), ["test-tag", "quoted tag"]);

        // Other top-level keys are not tags.
        let other = "colors:\n  red: 1\ntags:\n  one: null\n";
        assert_eq!(parse_tags_yml(other), ["one"]);
        assert!(parse_tags_yml("").is_empty());
    }

    #[test]
    fn tilde_expansion() {
        // Only `~/` is expanded, and only when HOME is set; this test reads
        // the real HOME, which exists under `cargo test`.
        if let Some(home) = home_dir() {
            assert_eq!(expand_tilde("~/x"), home.join("x"));
        }
        assert_eq!(expand_tilde("/abs/x"), PathBuf::from("/abs/x"));
    }
}

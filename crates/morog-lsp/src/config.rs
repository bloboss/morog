//! Server configuration from `initializationOptions` (plan §5.4).
//!
//! The client supplies the vault root and (optionally) the bibrat root; both
//! fall back to discovery — workspace folder for the vault, bibrat's own
//! resolution for the bibliography.

use std::path::PathBuf;

use lsp_types::InitializeParams;
use morog_sema::LintConfig;
use serde::Deserialize;

/// The shape of `initializationOptions`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct InitOptions {
    vault_root: Option<PathBuf>,
    bibrat_root: Option<PathBuf>,
    topic_max_words: Option<u32>,
}

/// Resolved server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// The vault the server analyzes and indexes.
    pub vault_root: PathBuf,
    /// Bibrat data directory; `None` means auto-discovery (plan §4).
    pub bibrat_root: Option<PathBuf>,
    pub lint: LintConfig,
    /// Whether the client accepts `RelativePattern` file watchers — the
    /// reliable way to watch the bibrat store outside the workspace folder.
    pub relative_pattern_support: bool,
}

/// The bibrat locations the server watches for live citekey freshness
/// (plan §5.4): the data directory and — in discovery mode — the bibrat
/// `config.toml` that names it.
#[derive(Debug, Clone, PartialEq)]
pub struct BibWatch {
    /// The data directory: the explicit `bibratRoot`, or bibrat's own
    /// resolution chain (config `data_dir`, `$BIBRAT_DATA_DIR`, XDG).
    pub root: PathBuf,
    /// `bibrat/config.toml` when discovery resolved the root; an explicit
    /// `bibratRoot` never reads the config, so there is nothing to watch.
    pub config: Option<PathBuf>,
}

impl BibWatch {
    /// Whether a watched-files path belongs to the bibrat store (entry
    /// dirs, `citation.bib`, `TAGS.yml`, or the config file itself).
    pub fn contains(&self, path: &std::path::Path) -> bool {
        path.starts_with(&self.root) || self.config.as_deref() == Some(path)
    }
}

/// Resolve the bibrat paths to watch. The paths need not exist yet —
/// watching them makes a store created later go live without a restart.
pub(crate) fn bib_watch(bibrat_root: Option<&std::path::Path>) -> Option<BibWatch> {
    match bibrat_root {
        Some(root) => Some(BibWatch {
            root: root.to_path_buf(),
            config: None,
        }),
        None => Some(BibWatch {
            root: morog_bib::resolve_data_dir()?,
            config: morog_bib::config_path(),
        }),
    }
}

impl Config {
    /// Read the configuration out of the initialize request: explicit
    /// options first, then the first workspace folder, then the process cwd.
    pub fn from_init(params: &InitializeParams) -> Self {
        let options: InitOptions = params
            .initialization_options
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        let vault_root = options
            .vault_root
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|folders| folders.first())
                    .and_then(|folder| folder.uri.to_file_path().ok())
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));

        let mut lint = LintConfig::default();
        if let Some(max) = options.topic_max_words {
            lint.topic_max_words = max;
        }

        let relative_pattern_support = params
            .capabilities
            .workspace
            .as_ref()
            .and_then(|w| w.did_change_watched_files.as_ref())
            .and_then(|w| w.relative_pattern_support)
            .unwrap_or(false);

        Self {
            vault_root,
            bibrat_root: options.bibrat_root,
            lint,
            relative_pattern_support,
        }
    }

    /// The bibrat paths this configuration watches (plan §5.4).
    pub fn bib_watch(&self) -> Option<BibWatch> {
        bib_watch(self.bibrat_root.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_options_win() {
        let params = InitializeParams {
            initialization_options: Some(serde_json::json!({
                "vaultRoot": "/vaults/research",
                "bibratRoot": "/data/bibrat",
                "topicMaxWords": 100,
            })),
            ..Default::default()
        };
        let config = Config::from_init(&params);
        assert_eq!(config.vault_root, PathBuf::from("/vaults/research"));
        assert_eq!(config.bibrat_root, Some(PathBuf::from("/data/bibrat")));
        assert_eq!(config.lint.topic_max_words, 100);
    }

    #[test]
    fn bib_watch_explicit_root_skips_the_config() {
        let watch = bib_watch(Some(std::path::Path::new("/data/bibrat"))).unwrap();
        assert_eq!(watch.root, PathBuf::from("/data/bibrat"));
        assert_eq!(watch.config, None);
        assert!(watch.contains(std::path::Path::new("/data/bibrat/papers/x/citation.bib")));
        assert!(watch.contains(std::path::Path::new("/data/bibrat/TAGS.yml")));
        assert!(!watch.contains(std::path::Path::new("/vault/note.md")));
    }

    #[test]
    fn workspace_folder_is_the_fallback() {
        let params = InitializeParams {
            workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
                uri: lsp_types::Url::from_file_path("/vaults/folder").unwrap(),
                name: "folder".into(),
            }]),
            ..Default::default()
        };
        let config = Config::from_init(&params);
        assert_eq!(config.vault_root, PathBuf::from("/vaults/folder"));
        assert_eq!(config.bibrat_root, None);
        assert_eq!(config.lint.topic_max_words, 500);
    }
}

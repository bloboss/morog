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

        Self {
            vault_root,
            bibrat_root: options.bibrat_root,
            lint,
        }
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

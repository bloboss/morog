//! morog's user configuration (plan §0, §6.1): a user-level
//! `$XDG_CONFIG_HOME/morog/config.toml` overridden **key by key** by a
//! vault-local `.morog.toml` at the vault root.
//!
//! v1 keys: `[publish] base_iri` (the IRI the published site lives under,
//! required by `morog publish`), `[publish] site_dir` (template-override
//! directory, plan §6.3), and `[lint] topic_max_words` (feeds
//! [`LintConfig`]). Unknown keys and sections are ignored — the plan
//! reserves `[kinds]` for a possible future (plan §0).
//!
//! This is a module in the CLI crate rather than a `morog-config` crate:
//! the CLI is the only consumer today (the LSP reads its configuration from
//! `initializationOptions`, plan §5.4), and promoting a self-contained
//! module to a crate later is mechanical.

use std::path::{Path, PathBuf};

use morog_sema::LintConfig;
use serde::Deserialize;

/// Name of the vault-local override file at the vault root.
pub const VAULT_FILE: &str = ".morog.toml";

/// The merged configuration (user file, then vault-local overrides).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub publish: PublishSection,
    pub lint: LintSection,
}

/// `[publish]` — the M8 exporter settings (plan §6.1, §6.3).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PublishSection {
    /// The IRI the published site lives under, e.g.
    /// `https://notes.example.com`. Public note IRIs are minted beneath it
    /// (`<base>/n/<uuid>`). No default: `morog publish` errors when unset.
    pub base_iri: Option<String>,
    /// Directory of template snippets overriding the embedded default theme
    /// (eleventy-style, plan §6.3). A relative path resolves against the
    /// directory of the file that set it.
    pub site_dir: Option<PathBuf>,
}

/// `[lint]` — knobs for [`LintConfig`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct LintSection {
    /// `sta::topic-too-long` threshold (default 500, plan §2.5).
    pub topic_max_words: Option<u32>,
}

impl Config {
    /// The [`LintConfig`] this configuration selects.
    pub fn lint_config(&self) -> LintConfig {
        let mut config = LintConfig::default();
        if let Some(max) = self.lint.topic_max_words {
            config.topic_max_words = max;
        }
        config
    }

    /// Overlay `vault` on `self`: every key the vault file sets wins.
    fn merged_with(mut self, vault: Config) -> Config {
        if vault.publish.base_iri.is_some() {
            self.publish.base_iri = vault.publish.base_iri;
        }
        if vault.publish.site_dir.is_some() {
            self.publish.site_dir = vault.publish.site_dir;
        }
        if vault.lint.topic_max_words.is_some() {
            self.lint.topic_max_words = vault.lint.topic_max_words;
        }
        self
    }
}

/// Load the configuration for a vault: the user file (when present) with
/// the vault-local `.morog.toml` (when present) layered on top.
///
/// Errors carry the offending path and are meant for the CLI user; a
/// missing file is not an error, a malformed one is.
pub fn load(vault_root: &Path) -> Result<Config, String> {
    let user = match user_config_path() {
        Some(path) => parse_file(&path)?.unwrap_or_default(),
        None => Config::default(),
    };
    let vault = parse_file(&vault_root.join(VAULT_FILE))?.unwrap_or_default();
    Ok(user.merged_with(vault))
}

/// `$XDG_CONFIG_HOME/morog/config.toml`, falling back to
/// `$HOME/.config/morog/config.toml` per the XDG Base Directory spec.
fn user_config_path() -> Option<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })?;
    Some(config_home.join("morog").join("config.toml"))
}

/// Parse one config file; `Ok(None)` when it does not exist. A `site_dir`
/// relative path is resolved against the file's own directory, so the key
/// means the same thing from either file.
fn parse_file(path: &Path) -> Result<Option<Config>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut config: Config =
        toml::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))?;
    if let (Some(dir), Some(parent)) = (&config.publish.site_dir, path.parent())
        && dir.is_relative()
    {
        config.publish.site_dir = Some(parent.join(dir));
    }
    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "morog-config-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn missing_files_mean_defaults() {
        let dir = TempDir::new("missing");
        assert_eq!(parse_file(&dir.0.join("none.toml")).unwrap(), None);
        let config = Config::default();
        assert_eq!(config.lint_config().topic_max_words, 500);
        assert_eq!(config.publish.base_iri, None);
    }

    #[test]
    fn parses_all_v1_keys_and_ignores_unknown_sections() {
        let dir = TempDir::new("parse");
        let path = write(
            &dir.0,
            "config.toml",
            "[publish]\nbase_iri = \"https://notes.example.com\"\nsite_dir = \"/abs/theme\"\n\n\
             [lint]\ntopic_max_words = 42\n\n[kinds]\nreserved = true\n",
        );
        let config = parse_file(&path).unwrap().unwrap();
        assert_eq!(
            config.publish.base_iri.as_deref(),
            Some("https://notes.example.com")
        );
        assert_eq!(
            config.publish.site_dir.as_deref(),
            Some(Path::new("/abs/theme"))
        );
        assert_eq!(config.lint_config().topic_max_words, 42);
    }

    #[test]
    fn relative_site_dir_resolves_against_the_config_file() {
        let dir = TempDir::new("sitedir");
        let path = write(&dir.0, ".morog.toml", "[publish]\nsite_dir = \"theme\"\n");
        let config = parse_file(&path).unwrap().unwrap();
        assert_eq!(config.publish.site_dir, Some(dir.0.join("theme")));
    }

    #[test]
    fn vault_file_wins_key_by_key() {
        let user: Config = toml::from_str(
            "[publish]\nbase_iri = \"https://user.example\"\nsite_dir = \"/user/theme\"\n\
             [lint]\ntopic_max_words = 100\n",
        )
        .unwrap();
        let vault: Config = toml::from_str("[lint]\ntopic_max_words = 9\n").unwrap();
        let merged = user.clone().merged_with(vault);
        // The vault file set only one key; the user file keeps the rest.
        assert_eq!(merged.lint.topic_max_words, Some(9));
        assert_eq!(
            merged.publish.base_iri.as_deref(),
            Some("https://user.example")
        );
        assert_eq!(
            merged.publish.site_dir.as_deref(),
            Some(Path::new("/user/theme"))
        );

        let vault: Config =
            toml::from_str("[publish]\nbase_iri = \"https://vault.example\"\n").unwrap();
        let merged = user.merged_with(vault);
        assert_eq!(
            merged.publish.base_iri.as_deref(),
            Some("https://vault.example")
        );
        assert_eq!(merged.lint.topic_max_words, Some(100));
    }

    #[test]
    fn malformed_toml_is_a_clear_error() {
        let dir = TempDir::new("badtoml");
        let path = write(&dir.0, ".morog.toml", "[publish\nbase_iri = 3\n");
        let err = parse_file(&path).unwrap_err();
        assert!(err.contains("invalid config"), "{err}");
        assert!(err.contains(".morog.toml"), "{err}");
    }
}

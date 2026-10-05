//! `morog init` — write a starter config file.
//!
//! By default writes the user-level `$XDG_CONFIG_HOME/morog/config.toml`
//! (the same path [`crate::config::load`] reads, via
//! [`morog_sema::tags::morog_config_path`], so init and loading can never
//! disagree). With `--vault [DIR]` it writes a vault-local `.morog.toml`
//! template instead. Either way an existing file is left untouched unless
//! `--force` — and "already exists" is still success, so installers can run
//! `morog init` unconditionally.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The user-level starter: the real default uncommented, everything else a
/// documented opt-in. Every key here is one `crate::config` actually reads.
const USER_TEMPLATE: &str = r#"# morog configuration
# Location: $XDG_CONFIG_HOME/morog/config.toml (~/.config when unset)
#
# A vault-local `.morog.toml` at the vault root overrides this file key by
# key: every key the vault file sets wins, anything unset falls through to
# this file (`morog init --vault` writes a starter). Unknown keys and
# sections are ignored.

[lint]
# `sta::topic-too-long` threshold, in body words.
topic_max_words = 500

[publish]
# The IRI the published site lives under; public note IRIs are minted
# beneath it (<base>/n/<uuid>). Required before `morog publish`.
# base_iri = "https://notes.example.com"

# Directory of template snippets overriding the embedded default theme;
# a relative path resolves against this file's directory.
# site_dir = "site"

# User-defined tags: declare how a custom tag's argument is interpreted,
# either a regex `pattern` with named capture groups or a `kind` shorthand
# (duration | date | timestamp | slug). `[tags]` alone also falls back to
# morg's own config (~/.config/morg/config.toml), lowest precedence.
# [tags.book]
# pattern = '"(?<title>[^"]+)"\s+by\s+(?<author>.+)'
# [tags.reading-time]
# kind = "duration"
"#;

/// The vault-local starter: all keys commented — overrides are opt-in.
const VAULT_TEMPLATE: &str = r#"# Vault-local morog overrides (.morog.toml at the vault root).
#
# Keys set here override the user config,
# $XDG_CONFIG_HOME/morog/config.toml, key by key: every key this file sets
# wins, anything unset falls through. Unknown keys and sections are
# ignored.

# [lint]
# topic_max_words = 500   # `sta::topic-too-long` threshold, in body words

# [publish]
# base_iri = "https://notes.example.com"  # required before `morog publish`
# site_dir = "site"       # theme overrides; relative to this file

# A tag declared here replaces the user/morg declaration wholesale.
# [tags.reading-time]
# kind = "duration"
"#;

pub fn run(vault: Option<PathBuf>, force: bool) -> ExitCode {
    match run_inner(vault, force) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("morog init: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(vault: Option<PathBuf>, force: bool) -> Result<String, String> {
    let (path, template) = match vault {
        Some(dir) => (dir.join(crate::config::VAULT_FILE), VAULT_TEMPLATE),
        None => (
            morog_sema::tags::morog_config_path()
                .ok_or("cannot locate the config directory (set XDG_CONFIG_HOME or HOME)")?,
            USER_TEMPLATE,
        ),
    };
    write_template(&path, template, force)
}

/// Write `template` to `path`, creating parent directories. An existing
/// file is left untouched (still `Ok`: nothing was asked to change) unless
/// `force`.
fn write_template(path: &Path, template: &str, force: bool) -> Result<String, String> {
    let existed = path.exists();
    if existed && !force {
        return Ok(format!(
            "config already exists at {}, left untouched (--force overwrites)",
            path.display()
        ));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, template).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(format!(
        "{} starter config at {}",
        if existed { "overwrote" } else { "created" },
        path.display()
    ))
}

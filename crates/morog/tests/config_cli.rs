//! Integration tests for the config files: user-level
//! `$XDG_CONFIG_HOME/morog/config.toml`, the vault-local `.morog.toml`
//! override, the `[tags]` fallback to morg's own
//! `$XDG_CONFIG_HOME/morg/config.toml`, and their wiring into `morog lint`.

use std::path::PathBuf;
use std::process::Output;

/// A verbose topic note: 12 body words, so any `topic_max_words` below that
/// trips `sta::topic-too-long` (the default 500 does not).
const WORDY_TOPIC: &str = "---\nid: wordy\nkind: topic\n---\n\nTwelve words of body text in a single evidence block right here [@key]. #anchor e1\n";

/// Per-test temp tree holding a fake config home and a vault.
struct TempSetup {
    root: PathBuf,
}

impl TempSetup {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "morog-config-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let setup = Self { root };
        std::fs::create_dir_all(setup.vault()).unwrap();
        std::fs::create_dir_all(setup.config_home().join("morog")).unwrap();
        std::fs::write(setup.vault().join("wordy.md"), WORDY_TOPIC).unwrap();
        setup
    }

    fn config_home(&self) -> PathBuf {
        self.root.join("config")
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    fn write_user_config(&self, text: &str) {
        std::fs::write(self.config_home().join("morog/config.toml"), text).unwrap();
    }

    fn write_vault_config(&self, text: &str) {
        std::fs::write(self.vault().join(".morog.toml"), text).unwrap();
    }

    /// morg's own config — the `[tags]` fallback layer.
    fn write_morg_config(&self, text: &str) {
        std::fs::create_dir_all(self.config_home().join("morg")).unwrap();
        std::fs::write(self.config_home().join("morg/config.toml"), text).unwrap();
    }

    /// Run `morog` from inside the vault with the fake config home.
    fn run(&self, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_morog"))
            .args(args)
            .current_dir(self.vault())
            .env("XDG_CONFIG_HOME", self.config_home())
            .output()
            .expect("run morog")
    }

    fn lint_stdout(&self) -> String {
        let out = self.run(&["lint", "--no-bib", "."]);
        String::from_utf8(out.stdout).unwrap()
    }
}

impl Drop for TempSetup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn default_topic_max_words_does_not_fire() {
    let setup = TempSetup::new("default");
    let stdout = setup.lint_stdout();
    assert!(!stdout.contains("sta::topic-too-long"), "{stdout}");
}

#[test]
fn user_config_sets_topic_max_words() {
    let setup = TempSetup::new("user");
    setup.write_user_config("[lint]\ntopic_max_words = 5\n");
    let stdout = setup.lint_stdout();
    assert!(stdout.contains("sta::topic-too-long"), "{stdout}");
    assert!(stdout.contains("(limit 5)"), "{stdout}");
}

#[test]
fn vault_config_overrides_the_user_file() {
    let setup = TempSetup::new("precedence");
    setup.write_user_config("[lint]\ntopic_max_words = 5\n");
    setup.write_vault_config("[lint]\ntopic_max_words = 1000\n");
    let stdout = setup.lint_stdout();
    // The vault-local file wins: the limit is back above the word count.
    assert!(!stdout.contains("sta::topic-too-long"), "{stdout}");

    // And the other way around: only the vault file sets a limit.
    let setup = TempSetup::new("vault-only");
    setup.write_vault_config("[lint]\ntopic_max_words = 5\n");
    let stdout = setup.lint_stdout();
    assert!(stdout.contains("(limit 5)"), "{stdout}");
}

/// A topic whose `#book` argument matches `pattern = "m.*"` but not
/// `pattern = "z.*"`, for steering `tag::shape-mismatch` per config layer.
const BOOK_TOPIC: &str = "---\nid: booky\nkind: topic\n---\n\nReading [@key]. #book mismatchable\n";

const MATCHING: &str = "[tags.book]\npattern = \"m.*\"\n";
const MISMATCHING: &str = "[tags.book]\npattern = \"z.*\"\n";

#[test]
fn morg_config_tags_are_honored_as_the_fallback_layer() {
    let setup = TempSetup::new("morg-fallback");
    std::fs::write(setup.vault().join("booky.md"), BOOK_TOPIC).unwrap();

    // Without any [tags], #book is an unknown tag: no mismatch lint.
    let stdout = setup.lint_stdout();
    assert!(!stdout.contains("tag::shape-mismatch"), "{stdout}");

    // A declaration written for morg alone drives morog's lint.
    setup.write_morg_config(MISMATCHING);
    let stdout = setup.lint_stdout();
    assert!(stdout.contains("tag::shape-mismatch"), "{stdout}");
    assert!(
        stdout.contains("argument does not match the declared pattern for #book"),
        "{stdout}"
    );
}

#[test]
fn tags_precedence_is_vault_over_morog_over_morg() {
    let setup = TempSetup::new("tags-precedence");
    std::fs::write(setup.vault().join("booky.md"), BOOK_TOPIC).unwrap();

    // morg says mismatch; the morog user config overrides to matching.
    setup.write_morg_config(MISMATCHING);
    setup.write_user_config(MATCHING);
    let stdout = setup.lint_stdout();
    assert!(!stdout.contains("tag::shape-mismatch"), "{stdout}");

    // The vault-local file wins over both: mismatch again.
    setup.write_vault_config(MISMATCHING);
    let stdout = setup.lint_stdout();
    assert!(stdout.contains("tag::shape-mismatch"), "{stdout}");
}

#[test]
fn invalid_tag_declarations_warn_and_keep_the_rest() {
    let setup = TempSetup::new("tags-invalid");
    std::fs::write(setup.vault().join("booky.md"), BOOK_TOPIC).unwrap();
    setup.write_vault_config("[tags.book]\npattern = \"z.*\"\n\n[tags.broken]\npattern = \"(\"\n");

    let out = setup.run(&["lint", "--no-bib", "."]);
    // The broken declaration is dropped with a warning on stderr…
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("broken"), "{stderr}");
    assert!(stderr.contains("declaration dropped"), "{stderr}");
    // …while the surviving declaration still lints.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("tag::shape-mismatch"), "{stdout}");
}

#[test]
fn malformed_config_is_a_clear_failure() {
    let setup = TempSetup::new("malformed");
    setup.write_vault_config("[lint\ntopic_max_words = \"many\"\n");
    let out = setup.run(&["lint", "--no-bib", "."]);
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("invalid config"), "{stderr}");
    assert!(stderr.contains(".morog.toml"), "{stderr}");
}

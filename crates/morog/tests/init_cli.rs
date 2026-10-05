//! Integration tests for `morog init`: the user-level starter at
//! `$XDG_CONFIG_HOME/morog/config.toml`, the vault-local `.morog.toml`
//! variant, the no-overwrite rule (success, for installer idempotency),
//! and `--force`. The round-trip check runs `morog lint` so the template
//! must load through the real config loader.

use std::path::PathBuf;
use std::process::Output;

/// Per-test temp tree holding a fake config home and a vault.
struct TempSetup {
    root: PathBuf,
}

impl TempSetup {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "morog-init-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let setup = Self { root };
        std::fs::create_dir_all(setup.vault()).unwrap();
        setup
    }

    fn config_home(&self) -> PathBuf {
        self.root.join("config")
    }

    fn user_config(&self) -> PathBuf {
        self.config_home().join("morog/config.toml")
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
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
}

impl Drop for TempSetup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn init_writes_a_user_config_that_loads_with_the_real_default() {
    let setup = TempSetup::new("roundtrip");
    // The config dir does not exist yet: init must create parents.
    let out = setup.run(&["init"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        stdout(&out).contains(&setup.user_config().display().to_string()),
        "{}",
        stdout(&out)
    );

    // The template is parseable TOML with the documented default set.
    let text = std::fs::read_to_string(setup.user_config()).unwrap();
    let value: toml::Value = toml::from_str(&text).unwrap();
    assert_eq!(
        value["lint"]["topic_max_words"].as_integer(),
        Some(500),
        "{text}"
    );

    // Round-trip through the real loader: a 501-word topic trips
    // `sta::topic-too-long` at exactly the template's limit, and the file
    // raises no config error or warning.
    let body = "word ".repeat(501);
    std::fs::write(
        setup.vault().join("long.md"),
        format!("---\nid: long\nkind: topic\n---\n\n{body}\n"),
    )
    .unwrap();
    let out = setup.run(&["lint", "--no-bib", "."]);
    let text = stdout(&out);
    assert!(text.contains("sta::topic-too-long"), "{text}");
    assert!(text.contains("(limit 500)"), "{text}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("invalid config"), "{stderr}");
}

#[test]
fn init_leaves_an_existing_config_untouched_and_still_succeeds() {
    let setup = TempSetup::new("no-overwrite");
    assert_eq!(setup.run(&["init"]).status.code(), Some(0));
    std::fs::write(setup.user_config(), "[lint]\ntopic_max_words = 7\n").unwrap();

    // Second init: exit 0 (installer idempotency) but no rewrite.
    let out = setup.run(&["init"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("already exists"), "{}", stdout(&out));
    let text = std::fs::read_to_string(setup.user_config()).unwrap();
    assert_eq!(text, "[lint]\ntopic_max_words = 7\n");
}

#[test]
fn init_force_overwrites() {
    let setup = TempSetup::new("force");
    assert_eq!(setup.run(&["init"]).status.code(), Some(0));
    std::fs::write(setup.user_config(), "[lint]\ntopic_max_words = 7\n").unwrap();

    let out = setup.run(&["init", "--force"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("overwrote"), "{}", stdout(&out));
    let text = std::fs::read_to_string(setup.user_config()).unwrap();
    assert!(text.contains("topic_max_words = 500"), "{text}");
}

#[test]
fn init_vault_writes_a_local_template_with_the_same_rules() {
    let setup = TempSetup::new("vault");
    // Bare `--vault` targets the current directory (the vault).
    let out = setup.run(&["init", "--vault"]);
    assert_eq!(out.status.code(), Some(0));
    let path = setup.vault().join(".morog.toml");
    assert!(path.exists());

    // Parseable TOML, and (all keys commented) it must pass through the
    // real loader without a config error.
    let text = std::fs::read_to_string(&path).unwrap();
    toml::from_str::<toml::Value>(&text).unwrap();
    std::fs::write(
        setup.vault().join("note.md"),
        "---\nid: n\nkind: topic\n---\n\nShort body [@key]. #anchor e1\n",
    )
    .unwrap();
    let out = setup.run(&["lint", "--no-bib", "."]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("invalid config"), "{stderr}");

    // No overwrite without --force; `--vault DIR` creates missing parents.
    std::fs::write(&path, "[lint]\ntopic_max_words = 7\n").unwrap();
    let out = setup.run(&["init", "--vault"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("already exists"), "{}", stdout(&out));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "[lint]\ntopic_max_words = 7\n"
    );

    let nested = setup.root.join("deep/new-vault");
    let nested_str = nested.display().to_string();
    let out = setup.run(&["init", "--vault", &nested_str]);
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    assert!(nested.join(".morog.toml").exists());
}

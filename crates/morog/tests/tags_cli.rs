//! Integration tests for `morog tags` (plan §10 T1): the occurrence table
//! comes from the index (built by `morog index` against the vault-local
//! `.morog.toml` `[tags]`), columns follow the declaration's group order,
//! and mismatches are flagged. Each test isolates `XDG_CONFIG_HOME` and
//! `XDG_CACHE_HOME` per child process.

use std::path::PathBuf;
use std::process::Output;

/// A vault-local `[tags]` declaration: `#book` with two named groups (in
/// pattern order: title, author) and `#pace` via the `duration` shorthand.
const VAULT_CONFIG: &str = "[tags.book]\npattern = '\"(?<title>[^\"]+)\" by (?<author>.+)'\n\n\
                            [tags.pace]\nkind = \"duration\"\n";

const READING: &str = "---\nid: reading\nkind: topic\n---\n\n\
                       Read [@wright2004]. #book \"Dune\" by Frank Herbert\n\n\
                       Also [@reich1997]. #book untitled notes\n";

const SHELF: &str = "---\nid: shelf\nkind: topic\n---\n\n\
                     Listed [@key]. #book \"Perilous Gard\" by Elizabeth Marie Pope\n";

/// Per-test temp tree holding a fake config home, cache home, and a vault.
struct TempSetup {
    root: PathBuf,
}

impl TempSetup {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "morog-tags-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let setup = Self { root };
        std::fs::create_dir_all(setup.vault().join("sub")).unwrap();
        std::fs::create_dir_all(setup.root.join("config")).unwrap();
        std::fs::create_dir_all(setup.root.join("cache")).unwrap();
        std::fs::write(setup.vault().join(".morog.toml"), VAULT_CONFIG).unwrap();
        std::fs::write(setup.vault().join("reading.md"), READING).unwrap();
        std::fs::write(setup.vault().join("sub/shelf.md"), SHELF).unwrap();
        setup
    }

    fn vault(&self) -> PathBuf {
        self.root.join("vault")
    }

    /// Run `morog` from inside the vault with the fake XDG homes.
    fn run(&self, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_morog"))
            .args(args)
            .current_dir(self.vault())
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .output()
            .expect("run morog")
    }

    fn index(&self) {
        let out = self.run(&["index", "--no-bib"]);
        assert_eq!(out.status.code(), Some(0), "{out:?}");
    }
}

impl Drop for TempSetup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn tags_requires_an_existing_index() {
    let setup = TempSetup::new("no-index");
    let out = setup.run(&["tags", "book"]);
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("run `morog index` first"), "{stderr}");
}

#[test]
fn tags_requires_a_declaration() {
    let setup = TempSetup::new("undeclared");
    setup.index();
    let out = setup.run(&["tags", "nosuch"]);
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no [tags] declaration"), "{stderr}");
}

#[test]
fn tags_tabulates_occurrences_from_the_index() {
    let setup = TempSetup::new("table");
    setup.index();
    let out = setup.run(&["tags", "book"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();

    // Columns in TagTable::group_names order (pattern order), between the
    // fixed file/line columns and the mismatch flag.
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        ["file", "line", "title", "author", "mismatch"]
    );
    // Rows ordered by file then line; the mismatching argument keeps its
    // row, with empty fields and the flag set.
    assert_eq!(
        lines[1].split_whitespace().collect::<Vec<_>>(),
        ["reading.md", "6", "Dune", "Frank", "Herbert"],
        "{stdout}"
    );
    assert_eq!(
        lines[2].split_whitespace().collect::<Vec<_>>(),
        ["reading.md", "8", "yes"],
        "{stdout}"
    );
    assert_eq!(
        lines[3].split_whitespace().collect::<Vec<_>>(),
        [
            "sub/shelf.md",
            "6",
            "Perilous",
            "Gard",
            "Elizabeth",
            "Marie",
            "Pope"
        ],
        "{stdout}"
    );
    assert!(stdout.contains("(3 rows)"), "{stdout}");
}

#[test]
fn tags_renders_json_with_group_fields() {
    let setup = TempSetup::new("json");
    setup.index();
    let out = setup.run(&["tags", "book", "--format", "json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");

    assert_eq!(rows[0]["file"], "reading.md");
    assert_eq!(rows[0]["line"], 6);
    assert_eq!(rows[0]["fields"]["title"], "Dune");
    assert_eq!(rows[0]["fields"]["author"], "Frank Herbert");
    assert_eq!(rows[0]["shape_mismatch"], false);

    assert_eq!(rows[1]["line"], 8);
    assert_eq!(rows[1]["fields"]["title"], serde_json::Value::Null);
    assert_eq!(rows[1]["shape_mismatch"], true);

    assert_eq!(rows[2]["file"], "sub/shelf.md");

    // The `kind` shorthand yields a single column named after the kind.
    let out = setup.run(&["tags", "pace", "--format", "json"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows, serde_json::json!([]));
}

//! Integration tests: `morog research` creation, listing, and checking in a
//! temporary vault, plus source creation from the bibrat fixture store.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use morog_sema::{ArgumentStatus, BloomOp, NoteMeta, SourceStatus, Stance, parse_note_meta};

fn fixture_bibrat() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bibrat")
}

fn fixture_vault() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault")
}

/// A fresh, empty vault under the target temp dir, unique per test.
struct TempVault(PathBuf);

impl TempVault {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("morog-research-{name}-{}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempVault {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_morog"))
        .args(args)
        .output()
        .expect("run morog")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).unwrap()
}

/// Parse a created file and assert the frontmatter round-trips through
/// morg-parser + parse_note_meta with zero MetaErrors.
fn retype(path: &Path) -> NoteMeta {
    let source = std::fs::read_to_string(path).unwrap();
    let parsed = morg_parser::parse_document(&source);
    let (meta, errors) = parse_note_meta(parsed.document.frontmatter.as_ref());
    assert!(
        errors.is_empty(),
        "{}: {errors:?}\n{source}",
        path.display()
    );
    meta
}

#[test]
fn topic_creation_round_trips_and_seeds_evidence() {
    let vault = TempVault::new("topic");
    let out = run(&[
        "research",
        "topic",
        "Specific Leaf Area",
        "--cite",
        "wright_leaf_2004",
        "--cite",
        "reich_global_1997",
        "--vault",
        vault.path().to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(stdout(&out).contains("created topic"));

    let file = vault.file("specific-leaf-area.md");
    let NoteMeta::Topic(m) = retype(&file) else {
        panic!("expected topic meta");
    };
    assert_eq!(m.id.as_ref().unwrap().as_str(), "specific-leaf-area");
    assert!(m.uuid.is_some());
    assert_eq!(m.title.as_deref(), Some("Specific Leaf Area"));
    assert!(m.created.is_some());
    assert!(!m.publish);

    // One evidence block per cited key, each with a stable anchor.
    let body = std::fs::read_to_string(&file).unwrap();
    assert!(
        body.contains("[@wright_leaf_2004]. #anchor evidence-1"),
        "{body}"
    );
    assert!(
        body.contains("[@reich_global_1997]. #anchor evidence-2"),
        "{body}"
    );

    // The created note lints clean (seeded blocks are evidence units).
    let lint = run(&["lint", "--no-bib", vault.path().to_str().unwrap()]);
    assert_eq!(lint.status.code(), Some(0), "{}", stdout(&lint));
}

#[test]
fn argument_creation_with_relations() {
    let vault = TempVault::new("argument");
    let v = vault.path().to_str().unwrap();
    run(&["research", "topic", "Squares", "--vault", v]);

    let out = run(&[
        "research",
        "argument",
        "Everything is allocation",
        "--to",
        "squares:supports:analyze",
        "--to",
        "squares:qualifies",
        "--vault",
        v,
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let NoteMeta::Argument(m) = retype(&vault.file("everything-is-allocation.md")) else {
        panic!("expected argument meta");
    };
    assert_eq!(m.id.as_ref().unwrap().as_str(), "everything-is-allocation");
    assert_eq!(m.status, Some(ArgumentStatus::Draft));
    assert!(!m.publish);
    assert_eq!(m.relations.len(), 2);
    assert_eq!(m.relations[0].target_id, "squares");
    assert_eq!(m.relations[0].stance, Stance::Supports);
    assert_eq!(m.relations[0].op, Some(BloomOp::Analyze));
    assert_eq!(m.relations[1].stance, Stance::Qualifies);
    assert_eq!(m.relations[1].op, None);

    // `research check` over the vault passes: both targets resolve.
    let check = run(&["research", "check", "--no-bib", v]);
    assert_eq!(check.status.code(), Some(0), "{}", stdout(&check));
    assert!(
        stdout(&check).contains("2 file(s) checked"),
        "{}",
        stdout(&check)
    );
}

#[test]
fn argument_to_flag_is_validated() {
    let vault = TempVault::new("to-validation");
    let v = vault.path().to_str().unwrap();

    let bad_stance = run(&["research", "argument", "X", "--to", "a:maybe", "--vault", v]);
    assert_eq!(bad_stance.status.code(), Some(1));
    assert!(
        stderr(&bad_stance).contains("supports, opposes, qualifies"),
        "{}",
        stderr(&bad_stance)
    );

    let bad_op = run(&[
        "research",
        "argument",
        "X",
        "--to",
        "a:supports:ponder",
        "--vault",
        v,
    ]);
    assert_eq!(bad_op.status.code(), Some(1));
    assert!(stderr(&bad_op).contains("recall"), "{}", stderr(&bad_op));

    let bad_shape = run(&[
        "research",
        "argument",
        "X",
        "--to",
        "just-an-id",
        "--vault",
        v,
    ]);
    assert_eq!(bad_shape.status.code(), Some(1));

    // No file was created by any failed attempt.
    assert!(!vault.file("x.md").exists());
}

#[test]
fn argument_rejects_source_targets() {
    let vault = TempVault::new("source-target");
    let v = vault.path().to_str().unwrap();
    let out = run(&[
        "research",
        "source",
        "paszke_pytorch_2019",
        "--bib-root",
        fixture_bibrat(),
        "--vault",
        v,
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let out = run(&[
        "research",
        "argument",
        "Straight at a source",
        "--to",
        "paszke_pytorch_2019:supports",
        "--vault",
        v,
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("arguments should go through topics"),
        "{}",
        stderr(&out)
    );
    assert!(!vault.file("straight-at-a-source.md").exists());
}

#[test]
fn duplicate_ids_are_refused() {
    let vault = TempVault::new("duplicate");
    let v = vault.path().to_str().unwrap();
    let first = run(&["research", "topic", "Leaf Mass Ratio", "--vault", v]);
    assert_eq!(first.status.code(), Some(0));

    // Same title (even punctuated differently) slugs to the same id.
    let second = run(&["research", "topic", "Leaf-Mass Ratio!", "--vault", v]);
    assert_eq!(second.status.code(), Some(1));
    assert!(
        stderr(&second).contains("id `leaf-mass-ratio` is already declared"),
        "{}",
        stderr(&second)
    );

    // A colliding file name is refused even if the id inside differs.
    std::fs::remove_file(vault.file("leaf-mass-ratio.md")).unwrap();
    std::fs::write(vault.file("leaf-mass-ratio.md"), "no frontmatter\n").unwrap();
    let third = run(&["research", "topic", "Leaf Mass Ratio", "--vault", v]);
    assert_eq!(third.status.code(), Some(1));
    assert!(
        stderr(&third).contains("cannot create"),
        "{}",
        stderr(&third)
    );
}

#[test]
fn source_from_bibrat_derives_title() {
    let vault = TempVault::new("source-bib");
    let v = vault.path().to_str().unwrap();
    let out = run(&[
        "research",
        "source",
        "paszke_pytorch_2019",
        "--bib-root",
        fixture_bibrat(),
        "--vault",
        v,
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let NoteMeta::Source(m) = retype(&vault.file("paszke_pytorch_2019.md")) else {
        panic!("expected source meta");
    };
    assert_eq!(m.id.as_ref().unwrap().as_str(), "paszke_pytorch_2019");
    assert_eq!(
        m.title.as_deref(),
        Some("PyTorch: An Imperative Style, High-Performance Deep Learning Library")
    );
    assert_eq!(m.status, Some(SourceStatus::Unread));
    assert!(m.uuid.is_some());

    // With the store in reach, the source note passes the bib-backed lints.
    let lint = run(&["lint", "--bib-root", fixture_bibrat(), v]);
    assert_eq!(lint.status.code(), Some(0), "{}", stdout(&lint));

    // An unknown key is refused.
    let unknown = run(&[
        "research",
        "source",
        "nope_2020",
        "--bib-root",
        fixture_bibrat(),
        "--vault",
        v,
    ]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        stderr(&unknown).contains("not found in the bibrat store"),
        "{}",
        stderr(&unknown)
    );
}

#[test]
fn source_without_bibliography_still_creates() {
    let vault = TempVault::new("source-nobib");
    let v = vault.path().to_str().unwrap();
    let out = run(&[
        "research",
        "source",
        "reich_global_1997",
        "--no-bib",
        "--vault",
        v,
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let NoteMeta::Source(m) = retype(&vault.file("reich_global_1997.md")) else {
        panic!("expected source meta");
    };
    assert_eq!(m.title, None); // derived from bibrat only when available
    assert_eq!(m.status, Some(SourceStatus::Unread));
}

#[test]
fn list_tabulates_by_kind_status_title() {
    let vault = TempVault::new("list");
    let v = vault.path().to_str().unwrap();
    run(&["research", "topic", "Specific Leaf Area", "--vault", v]);
    run(&[
        "research",
        "argument",
        "An Argument",
        "--to",
        "specific-leaf-area:supports",
        "--vault",
        v,
    ]);
    run(&[
        "research",
        "source",
        "wright_leaf_2004",
        "--no-bib",
        "--vault",
        v,
    ]);
    // A plain morg note is not an STA note and is not listed.
    std::fs::write(vault.file("scratch.md"), "---\ntags: [daily]\n---\n\nHi.\n").unwrap();

    let out = run(&["research", "list", v]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("argument  draft"), "{text}");
    assert!(text.contains("topic     -"), "{text}");
    assert!(text.contains("source    unread"), "{text}");
    assert!(text.contains("Specific Leaf Area"), "{text}");
    assert!(!text.contains("scratch"), "{text}");
    assert!(
        text.contains("3 note(s): 1 source, 1 topic, 1 argument"),
        "{text}"
    );

    let json = run(&["--format", "json", "research", "list", v]);
    let text = stdout(&json);
    assert!(text.trim_start().starts_with('['), "{text}");
    assert!(
        text.contains(r#""kind": "topic", "status": null"#),
        "{text}"
    );
    assert!(text.contains(r#""id": "an-argument""#), "{text}");
    assert!(!text.contains("scratch"), "{text}");
}

#[test]
fn check_is_sta_restricted_lint() {
    // Over the committed fixture vault: same diagnostics surface as lint,
    // but plain.md is excluded from the pass and the file count.
    let out = run(&["research", "check", "--no-bib", fixture_vault()]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("sta::relation-target"), "{text}");
    assert!(text.contains("sta::kind: unknown kind `journal`"), "{text}");
    assert!(text.contains("8 file(s) checked"), "{text}");

    let lint = run(&["lint", "--no-bib", fixture_vault()]);
    assert!(
        stdout(&lint).contains("9 file(s) checked"),
        "{}",
        stdout(&lint)
    );

    // JSON output matches the lint format.
    let json = run(&[
        "--format",
        "json",
        "research",
        "check",
        "--no-bib",
        fixture_vault(),
    ]);
    assert_eq!(json.status.code(), Some(1));
    let text = stdout(&json);
    assert!(text.contains(r#""code": "sta::relation-target""#), "{text}");
    assert!(!text.contains("file(s) checked"), "{text}");

    // A clean vault exits zero.
    let vault = TempVault::new("check-clean");
    let v = vault.path().to_str().unwrap();
    run(&["research", "topic", "Clean", "--vault", v]);
    let clean = run(&["research", "check", "--no-bib", v]);
    assert_eq!(clean.status.code(), Some(0), "{}", stdout(&clean));
}

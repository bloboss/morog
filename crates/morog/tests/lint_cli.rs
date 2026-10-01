//! Integration test: `morog lint` over the committed fixture vault.

use std::path::Path;
use std::process::{Command, Output};

fn fixture_vault() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault")
}

fn run_lint(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_morog"))
        .args(args)
        .output()
        .expect("run morog lint")
}

#[test]
fn lint_fixture_vault_text_output() {
    let out = run_lint(&["lint", "--no-bib", fixture_vault()]);
    let stdout = String::from_utf8(out.stdout).unwrap();

    // Error-severity diagnostics are present, so the exit code is nonzero.
    assert_eq!(out.status.code(), Some(1), "stdout:\n{stdout}");

    let expect = |needle: &str| {
        assert!(stdout.contains(needle), "missing `{needle}` in:\n{stdout}");
    };

    // sta::kind — unknown kind value and unknown frontmatter key.
    expect("bad-kind.md:3:7: sta::kind: unknown kind `journal`");
    expect("leaf-mass-ratio.md:6:1: sta::kind: unknown frontmatter key `colour`");

    // sta::uncited-block — the uncited paragraph, located by line.
    expect("leaf-mass-ratio.md:13:1: sta::uncited-block:");

    // sta::argument-arity — weak-arg declares a single relation.
    expect("weak-arg.md:1:1: sta::argument-arity:");

    // sta::relation-target — argument pointing at a source, and a dangling id.
    expect("weak-arg.md:9:11: sta::relation-target:");
    expect("broken-targets.md:9:11: sta::relation-target: relation target `no-such-topic`");

    // sta::positional-address — `specific-leaf-area#b2`.
    expect("broken-targets.md:10:11: sta::positional-address:");

    // sta::id — duplicate across weak-arg.md and weak-arg-copy.md.
    expect("weak-arg.md:2:5: sta::id: id `weak-arg` is also declared in");
    expect("weak-arg-copy.md:2:5: sta::id: id `weak-arg` is also declared in");

    // Clean notes produce no diagnostics.
    assert!(!stdout.contains("specific-leaf-area.md:"), "{stdout}");
    assert!(!stdout.contains("two-shape-classes.md:"), "{stdout}");
    assert!(!stdout.contains("plain.md:"), "{stdout}");
    assert!(!stdout.contains("paszke_pytorch_2019.md:"), "{stdout}");

    // Summary line.
    expect("9 file(s) checked:");
}

#[test]
fn lint_honors_json_format() {
    let out = run_lint(&["--format", "json", "lint", "--no-bib", fixture_vault()]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();

    assert!(stdout.trim_start().starts_with('['));
    assert!(stdout.trim_end().ends_with(']'));
    assert!(stdout.contains(r#""code": "sta::positional-address""#));
    assert!(stdout.contains(r#""severity": "warning""#));
    assert!(stdout.contains(r#""severity": "error""#));
    assert!(stdout.contains(r#""line": 9"#));
    // No text summary in JSON mode.
    assert!(!stdout.contains("file(s) checked"));
}

#[test]
fn lint_single_clean_file_exits_zero() {
    let path = Path::new(fixture_vault()).join("topics/specific-leaf-area.md");
    let out = run_lint(&["lint", "--no-bib", path.to_str().unwrap()]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "stdout:\n{stdout}");
    assert!(stdout.contains("1 file(s) checked: 0 error(s), 0 warning(s)"));
}

fn fixture_bibrat() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bibrat")
}

#[test]
fn lint_with_bib_root_checks_citekeys_and_source_ids() {
    let out = run_lint(&["lint", "--bib-root", fixture_bibrat(), fixture_vault()]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "stdout:\n{stdout}");

    // sta::unknown-citekey — poorter_biomass_2012 is not in the store.
    assert!(
        stdout.contains(
            "leaf-mass-ratio.md:11:1: sta::unknown-citekey: unknown citation key \
             `@poorter_biomass_2012`"
        ),
        "{stdout}"
    );
    // … while the keys the store does hold resolve silently.
    assert!(!stdout.contains("wright_leaf_2004"), "{stdout}");
    assert!(!stdout.contains("reich_global_1997"), "{stdout}");
    // sta::id — the source note's id matches its bibliography key.
    assert!(!stdout.contains("paszke_pytorch_2019.md:"), "{stdout}");
}

#[test]
fn lint_source_id_mismatch_fires_with_store() {
    // Against an empty-but-valid store every citekey and source id is
    // unknown; the source fixture then trips the id-matches-key lint.
    let empty = std::env::temp_dir().join(format!("morog-lint-empty-bib-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let out = run_lint(&[
        "lint",
        "--bib-root",
        empty.to_str().unwrap(),
        fixture_vault(),
    ]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let _ = std::fs::remove_dir_all(&empty);
    assert!(
        stdout.contains(
            "paszke_pytorch_2019.md:2:5: sta::id: source id `paszke_pytorch_2019` \
             does not match any bibliography key"
        ),
        "{stdout}"
    );
}

#[test]
fn lint_bib_root_missing_directory_fails() {
    let out = run_lint(&["lint", "--bib-root", "/nonexistent/bibrat", fixture_vault()]);
    assert_ne!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("does not exist"), "{stderr}");
}

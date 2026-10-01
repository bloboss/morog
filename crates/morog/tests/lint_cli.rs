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
    let out = run_lint(&["lint", fixture_vault()]);
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
    let out = run_lint(&["--format", "json", "lint", fixture_vault()]);
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
    let out = run_lint(&["lint", path.to_str().unwrap()]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "stdout:\n{stdout}");
    assert!(stdout.contains("1 file(s) checked: 0 error(s), 0 warning(s)"));
}

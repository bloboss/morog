//! Integration tests: `morog index` and `morog query` over the committed
//! fixture vault (and the generated bench vault, when present).

use std::path::{Path, PathBuf};
use std::process::Output;

fn fixture_vault() -> &'static str {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/vault")
}

/// Temp directory (per test) that doubles as `XDG_CACHE_HOME`, so test
/// indexes never touch the real cache. Cleans up after itself.
struct TempHome(PathBuf);

impl TempHome {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "morog-cli-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_morog"))
            .args(args)
            .current_dir(cwd)
            .env("XDG_CACHE_HOME", self.path())
            .output()
            .expect("run morog")
    }

    /// Run and require success, returning stdout.
    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run(cwd, args);
        assert!(
            out.status.success(),
            "`morog {}` failed:\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn query(&self, cwd: &Path, sql: &str) -> String {
        self.ok(cwd, &["query", sql])
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn index_and_query_fixture_vault() {
    let home = TempHome::new("fixture");
    let vault = Path::new(fixture_vault());

    // Index with the vault as cwd (the default), then rescan explicitly.
    let out = home.ok(vault, &["index"]);
    assert!(out.contains("9 scanned"), "stdout: {out}");
    assert!(out.contains("9 indexed"), "stdout: {out}");

    let out = home.ok(vault, &["--format", "json", "index", "."]);
    assert!(out.contains("\"scanned\": 9"), "stdout: {out}");
    assert!(out.contains("\"changed\": 0"), "stdout: {out}");
    assert!(out.contains("\"indexed\": 0"), "stdout: {out}");

    // 7 kinded notes, minus one duplicate id; 2 plain files (no kind /
    // unknown kind).
    let out = home.query(vault, "SELECT COUNT(*) AS n FROM notes");
    assert!(out.contains("6"), "notes: {out}");
    let out = home.query(vault, "SELECT COUNT(*) AS n FROM plain");
    assert!(out.contains("2"), "plain: {out}");

    // Resolved edges: 7 argues + 1 related + 2 mentions (see fixtures).
    let out = home.query(vault, "SELECT COUNT(*) AS n FROM v_edges");
    assert!(out.contains("10"), "v_edges: {out}");

    // Dangling: one unknown note, one unknown anchor, three cites with no
    // bib store behind them (bib_entries is empty in M4).
    let out = home.query(
        vault,
        "SELECT dst, reason FROM v_dangling ORDER BY dst, reason",
    );
    assert!(
        out.contains("no-such-topic") && out.contains("unknown-note"),
        "v_dangling: {out}"
    );
    assert!(
        out.contains("specific-leaf-area") && out.contains("unknown-anchor"),
        "v_dangling: {out}"
    );
    assert!(out.contains("(5 rows)"), "v_dangling: {out}");

    // Orphans: the source is never cited; every topic has an argument.
    let out = home.query(vault, "SELECT note, kind FROM v_orphans");
    assert!(out.contains("paszke_pytorch_2019"), "v_orphans: {out}");
    assert!(out.contains("(1 row)"), "v_orphans: {out}");

    // Evidence blocks carry their citekeys and locators.
    let out = home.query(
        vault,
        "SELECT topic, anchor, citekey, locator FROM v_evidence ORDER BY citekey",
    );
    assert!(out.contains("wright_leaf_2004"), "v_evidence: {out}");
    assert!(out.contains("p. 822"), "v_evidence: {out}");
    assert!(out.contains("(3 rows)"), "v_evidence: {out}");

    // Bloom depth is MAX(op): two-shape-classes peaks at evaluate (4).
    let out = home.ok(
        vault,
        &[
            "--format",
            "json",
            "query",
            "SELECT argument, depth FROM v_argument_depth ORDER BY argument",
        ],
    );
    assert!(
        out.contains("\"argument\": \"two-shape-classes\", \"depth\": 4"),
        "v_argument_depth: {out}"
    );

    // Full-text search reaches block text.
    let out = home.query(
        vault,
        "SELECT COUNT(*) AS n FROM blocks_fts WHERE blocks_fts MATCH 'economics'",
    );
    assert!(out.contains("1"), "fts: {out}");
}

#[test]
fn query_rejects_writes_and_missing_index() {
    let home = TempHome::new("rejects");
    let vault = Path::new(fixture_vault());

    // No index yet: a helpful error, nonzero exit.
    let out = home.run(vault, &["query", "SELECT 1"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("run `morog index` first"), "{stderr}");

    home.ok(vault, &["index"]);

    for sql in [
        "DELETE FROM files",
        "UPDATE notes SET title = 'x'",
        "PRAGMA journal_mode = DELETE",
        "SELECT 1; DROP TABLE notes",
    ] {
        let out = home.run(vault, &["query", sql]);
        assert!(!out.status.success(), "`{sql}` should have been rejected");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("only a single SELECT"),
            "`{sql}` stderr: {stderr}"
        );
    }
}

/// Smoke test over the generated benchmark vault (bench/vault is
/// gitignored; regenerate with `cargo run --release -p morog-bench -- gen`).
/// Skips gracefully when absent.
#[test]
fn index_bench_vault_smoke() {
    let bench_vault = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/vault"));
    if !bench_vault.is_dir() {
        eprintln!("bench/vault not generated; skipping smoke test");
        return;
    }
    let home = TempHome::new("bench");

    let out = home.ok(bench_vault, &["--format", "json", "index"]);
    let scanned: usize = out
        .split("\"scanned\": ")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
        .expect("scanned count in json output");
    assert!(scanned > 1000, "suspiciously small bench vault: {out}");

    // Warm rescan sees no changes.
    let out = home.ok(bench_vault, &["--format", "json", "index"]);
    assert!(out.contains("\"changed\": 0"), "stdout: {out}");

    let rows = home.query(bench_vault, "SELECT COUNT(*) AS n FROM notes");
    assert!(!rows.is_empty());
}

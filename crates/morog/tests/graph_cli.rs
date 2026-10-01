//! Integration tests: `morog graph` over the committed fixture vault —
//! the plan §7.1 rendering table, the filter flags, the evidence clusters,
//! and (when graphviz is installed) that the emitted DOT actually parses.

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
            "morog-graph-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A fresh home with the fixture vault already indexed (`--no-bib`,
    /// so the run does not depend on any bibrat store on the machine).
    fn indexed(tag: &str) -> Self {
        let home = Self::new(tag);
        home.ok(Path::new(fixture_vault()), &["index", "--no-bib"]);
        home
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

    fn graph(&self, args: &[&str]) -> String {
        let mut full = vec!["graph"];
        full.extend_from_slice(args);
        self.ok(Path::new(fixture_vault()), &full)
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn graph_renders_the_plan_table_over_the_fixture_vault() {
    let home = TempHome::indexed("render");
    let out = home.run(Path::new(fixture_vault()), &["graph"]);
    assert!(out.status.success());
    let dot = String::from_utf8(out.stdout).unwrap();

    assert!(dot.starts_with("digraph morog {"), "{dot}");
    assert!(dot.contains("rankdir=BT"), "{dot}");

    // Node shapes per kind.
    assert!(
        dot.contains("\"paszke_pytorch_2019\" [label=\"Paszke 2019\", shape=note]"),
        "{dot}"
    );
    assert!(
        dot.contains("\"specific-leaf-area\" [label=\"Specific leaf area\", shape=ellipse]"),
        "{dot}"
    );
    // Argument hexagons filled by Bloom depth: two-shape-classes peaks at
    // evaluate (4, a dark shade with white text); broken-targets has no
    // typed relation and stays white.
    assert!(
        dot.contains(
            "\"two-shape-classes\" [label=\"Leaf economics is allocation in disguise\", \
             shape=hexagon, style=filled, fillcolor=\"#3182bd\", fontcolor=\"#ffffff\"]"
        ),
        "{dot}"
    );
    assert!(
        dot.contains("\"broken-targets\" [label=\"Dangling and positional targets\", shape=hexagon, style=filled, fillcolor=\"#ffffff\"]"),
        "{dot}"
    );

    // A supports edge: green, solid, op as label.
    assert!(
        dot.contains("\"weak-arg\" -> \"paszke_pytorch_2019\" [color=\"#1a7f37\", style=solid]"),
        "{dot}"
    );
    // An opposes edge: red, dashed.
    assert!(
        dot.contains(
            "\"weak-arg\" -> \"leaf-mass-ratio\" [color=\"#cf222e\", style=dashed, \
             label=\"evaluate\"]"
        ),
        "{dot}"
    );
    // A qualifies edge: orange, dotted.
    assert!(
        dot.contains(
            "\"two-shape-classes\" -> \"specific-leaf-area\" [color=\"#e36209\", \
             style=dotted, label=\"evaluate\"]"
        ),
        "{dot}"
    );

    // Dangling relations are skipped but counted on stderr: the unknown
    // note target plus three unresolved citekeys (indexed with --no-bib).
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("skipped 4 dangling relations"), "{stderr}");
    assert!(!dot.contains("no-such-topic"), "{dot}");
}

#[test]
fn root_and_depth_restrict_to_the_neighborhood() {
    let home = TempHome::indexed("root");

    // The source is two hops from specific-leaf-area (via weak-arg).
    let dot = home.graph(&["--root", "specific-leaf-area", "--depth", "1"]);
    assert!(dot.contains("\"specific-leaf-area\""), "{dot}");
    assert!(dot.contains("\"two-shape-classes\""), "{dot}");
    assert!(!dot.contains("paszke_pytorch_2019"), "{dot}");

    let dot = home.graph(&["--root", "specific-leaf-area", "--depth", "2"]);
    assert!(dot.contains("paszke_pytorch_2019"), "{dot}");

    // --root alone defaults to one hop.
    let dot = home.graph(&["--root", "specific-leaf-area"]);
    assert!(!dot.contains("paszke_pytorch_2019"), "{dot}");

    // An unknown root is a clear error.
    let out = home.run(Path::new(fixture_vault()), &["graph", "--root", "nope"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("root `nope` is not in the graph"),
        "{stderr}"
    );
}

#[test]
fn evidence_expands_topics_into_clusters() {
    let home = TempHome::indexed("evidence");
    let dot = home.graph(&["--evidence"]);

    // One cluster per topic with evidence, labelled by title, containing
    // the topic node and its evidence blocks (anchor + truncated text).
    assert!(
        dot.contains("subgraph \"cluster_specific-leaf-area\""),
        "{dot}"
    );
    assert!(
        dot.contains("subgraph \"cluster_leaf-mass-ratio\""),
        "{dot}"
    );
    assert!(dot.contains("label=\"Specific leaf area\";"), "{dot}");
    assert!(
        dot.contains(
            "\"specific-leaf-area#evidence-1\" [label=\"evidence-1\\nSLA is projected \
             leaf area per unit dry…\", shape=box, fontsize=10]"
        ),
        "{dot}"
    );

    // Relations targeting id#anchor land on the evidence node.
    assert!(
        dot.contains(
            "\"two-shape-classes\" -> \"specific-leaf-area#evidence-1\" \
             [color=\"#1a7f37\", style=solid, label=\"analyze\"]"
        ),
        "{dot}"
    );
    // A dangling anchor (#b2) falls back to the topic node itself.
    assert!(
        dot.contains("\"broken-targets\" -> \"specific-leaf-area\" ["),
        "{dot}"
    );
    // Clusters are emitted exactly once each.
    assert_eq!(dot.matches("subgraph").count(), 2, "{dot}");
}

#[test]
fn edge_and_node_filters_apply() {
    let home = TempHome::indexed("filters");

    // --min-op keeps only edges at least that deep (apply < evaluate).
    let dot = home.graph(&["--min-op", "evaluate"]);
    assert!(dot.contains("label=\"evaluate\""), "{dot}");
    assert!(!dot.contains("label=\"apply\""), "{dot}");
    assert!(!dot.contains("label=\"recall\""), "{dot}");

    // --stance keeps only matching edges; unstanced edges drop too.
    let dot = home.graph(&["--stance", "opposes"]);
    assert!(dot.contains("style=dashed"), "{dot}");
    assert!(!dot.contains("style=dotted"), "{dot}");
    assert!(!dot.contains("#999999"), "{dot}"); // related/mentions gone

    // --kinds drops nodes (and their edges) of other kinds.
    let dot = home.graph(&["--kinds", "topic,argument"]);
    assert!(!dot.contains("shape=note"), "{dot}");
    assert!(dot.contains("shape=hexagon"), "{dot}");

    // --published: no fixture note sets publish, so the graph is empty
    // but still valid DOT.
    let dot = home.graph(&["--published"]);
    assert!(dot.contains("digraph morog"), "{dot}");
    assert!(!dot.contains("shape="), "{dot}");

    // Unknown filter values are clear errors.
    for (args, needle) in [
        (
            vec!["graph", "--kinds", "journal"],
            "unknown kind `journal`",
        ),
        (vec!["graph", "--stance", "maybe"], "unknown stance `maybe`"),
        (vec!["graph", "--min-op", "ponder"], "unknown op `ponder`"),
    ] {
        let out = home.run(Path::new(fixture_vault()), &args);
        assert!(!out.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(needle), "{args:?}: {stderr}");
    }
}

#[test]
fn output_file_and_json_format() {
    let home = TempHome::indexed("output");
    let file = home.path().join("graph.dot");

    let stdout = home.graph(&["-o", file.to_str().unwrap()]);
    assert!(stdout.is_empty(), "{stdout}");
    let dot = std::fs::read_to_string(&file).unwrap();
    assert!(dot.contains("digraph morog"), "{dot}");

    let json = home.ok(
        Path::new(fixture_vault()),
        &["--format", "json", "graph", "--evidence"],
    );
    assert!(json.contains("\"nodes\": ["), "{json}");
    assert!(
        json.contains("{\"id\": \"two-shape-classes\", \"kind\": \"argument\", "),
        "{json}"
    );
    assert!(json.contains("\"depth\": 4"), "{json}");
    assert!(
        json.contains(
            "{\"src\": \"two-shape-classes\", \"dst\": \"specific-leaf-area#evidence-1\", \
             \"kind\": \"argues\", \"stance\": \"supports\", \"op\": \"analyze\"}"
        ),
        "{json}"
    );
}

#[test]
fn graph_requires_an_existing_index() {
    let home = TempHome::new("noindex"); // no `morog index` run
    let out = home.run(Path::new(fixture_vault()), &["graph"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("run `morog index` first"), "{stderr}");
}

/// The emitted DOT parses with graphviz when it is installed; skips
/// gracefully (and says so) otherwise.
#[test]
fn emitted_dot_parses_with_graphviz() {
    use std::io::Write as _;

    if std::process::Command::new("dot")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("graphviz not installed; skipping dot validation");
        return;
    }

    let home = TempHome::indexed("dotcheck");
    for args in [&[][..], &["--evidence"][..]] {
        let dot = home.graph(args);
        let mut child = std::process::Command::new("dot")
            .args(["-Tsvg", "-o", "/dev/null"])
            .stdin(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn dot");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(dot.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "dot rejected `morog graph {}` output:\n{}\n{dot}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    eprintln!("dot validation ran (graphviz installed)");
}

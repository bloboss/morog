//! The M4 property test (plan §5.4): `FileFacts` round-trip losslessly
//! through SQLite rows — `extract(file) == load(store(extract(file)))`.
//!
//! A hand-rolled deterministic generator (no external proptest dependency)
//! produces note sources over the representative shapes: all three STA
//! kinds plus plain morg notes, anchors, `#mine` tags, cites with and
//! without locators, typed relations with every stance and op, `related:`
//! lists, aliases, `id:` links, headings/lists/code blocks, missing ids,
//! and multi-byte text in every string position. Coverage flags assert the
//! corpus really exercised each feature.

use morg_parser::parse_document;
use morog_sema::FileFacts;
use rusqlite::Connection;

use morog_index::{load_file_facts, schema, store_file_facts};

// ---------------------------------------------------------------------------
// splitmix64, as in morog-bench: deterministic without external crates.
// ---------------------------------------------------------------------------

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

// ---------------------------------------------------------------------------
// Source generator
// ---------------------------------------------------------------------------

/// Which features a generated document exercised, so the test can prove the
/// corpus covered them all.
#[derive(Default, Debug)]
struct Coverage {
    source: bool,
    topic: bool,
    argument: bool,
    plain: bool,
    plain_with_id: bool,
    missing_id: bool,
    uuid: bool,
    aliases: bool,
    related: bool,
    related_anchor: bool,
    relations: bool,
    relation_anchor: bool,
    relation_without_op: bool,
    every_stance: [bool; 3],
    every_op: [bool; 6],
    anchor: bool,
    mine: bool,
    cite: bool,
    cite_locator: bool,
    id_link: bool,
    id_link_anchor: bool,
    multibyte: bool,
    heading: bool,
    list: bool,
    code_block: bool,
    empty_body: bool,
    publish: bool,
    thesis: bool,
    question_form: bool,
    created: bool,
    extra: bool,
}

impl Coverage {
    fn assert_complete(&self) {
        assert!(self.source, "corpus never generated a source note");
        assert!(self.topic, "corpus never generated a topic note");
        assert!(self.argument, "corpus never generated an argument note");
        assert!(self.plain, "corpus never generated a plain note");
        assert!(self.plain_with_id, "no plain note with an id: key");
        assert!(self.missing_id, "no kinded note with a missing id");
        assert!(self.uuid, "no note with a uuid");
        assert!(self.aliases, "no topic with aliases");
        assert!(self.related, "no topic with related:");
        assert!(self.related_anchor, "no related: target with #anchor");
        assert!(self.relations, "no argument with relations");
        assert!(self.relation_anchor, "no relation target with #anchor");
        assert!(self.relation_without_op, "no relation without op");
        assert!(self.every_stance.iter().all(|b| *b), "missing a stance");
        assert!(self.every_op.iter().all(|b| *b), "missing a bloom op");
        assert!(self.anchor, "no block #anchor");
        assert!(self.mine, "no #mine block");
        assert!(self.cite, "no inline cite");
        assert!(self.cite_locator, "no cite with locator");
        assert!(self.id_link, "no id: link");
        assert!(self.id_link_anchor, "no id: link with anchor");
        assert!(self.multibyte, "no multi-byte text");
        assert!(self.heading, "no heading block");
        assert!(self.list, "no list block");
        assert!(self.code_block, "no code block");
        assert!(self.empty_body, "no empty-body note");
        assert!(self.publish, "no published note");
        assert!(self.thesis, "no argument with thesis");
        assert!(self.question_form, "no question-form argument");
        assert!(self.created, "no note with created:");
        assert!(self.extra, "no note with an extra: map");
    }
}

const STANCES: [&str; 3] = ["supports", "opposes", "qualifies"];
const OPS: [&str; 6] = [
    "recall", "explain", "apply", "analyze", "evaluate", "create",
];
const ASCII_WORDS: [&str; 8] = [
    "leaf",
    "allocation",
    "spectrum",
    "biomass",
    "axis",
    "trait",
    "canopy",
    "shade",
];
const MULTIBYTE_WORDS: [&str; 6] = [
    "café",
    "naïve",
    "日本語のメモ",
    "βιβλίο",
    "🦀🔬",
    "Überschrift",
];
const CITE_KEYS: [&str; 4] = [
    "wright_leaf_2004",
    "reich_global_1997",
    "poorter_biomass_2012",
    "paszke_pytorch_2019",
];
const LOCATORS: [&str; 3] = ["p. 822", "ch. 3", "§4 ¶2"];
const TARGETS: [&str; 5] = [
    "specific-leaf-area",
    "leaf-mass-ratio",
    "no-such-topic",
    "circles#evidence-2",
    "x#",
];

fn words(rng: &mut Rng, n: usize, cov: &mut Coverage) -> String {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        if rng.chance(25) {
            cov.multibyte = true;
            out.push(*rng.pick(&MULTIBYTE_WORDS));
        } else {
            out.push(*rng.pick(&ASCII_WORDS));
        }
    }
    out.join(" ")
}

fn gen_paragraph(rng: &mut Rng, i: usize, cov: &mut Coverage) -> String {
    let n = 3 + rng.below(8);
    let mut p = words(rng, n, cov);
    if rng.chance(50) {
        let key = *rng.pick(&CITE_KEYS);
        cov.cite = true;
        if rng.chance(50) {
            let locator = *rng.pick(&LOCATORS);
            cov.cite_locator = true;
            p.push_str(&format!(" [@{key}, {locator}]"));
        } else {
            p.push_str(&format!(" [@{key}]"));
        }
    }
    if rng.chance(35) {
        let target = *rng.pick(&TARGETS[..3]);
        cov.id_link = true;
        if rng.chance(50) {
            cov.id_link_anchor = true;
            p.push_str(&format!(" see [that](id:{target}#evidence-1)"));
        } else {
            p.push_str(&format!(" see [that](id:{target})"));
        }
    }
    if rng.chance(30) {
        cov.mine = true;
        p.push_str(" #mine");
    }
    if rng.chance(40) {
        cov.anchor = true;
        p.push_str(&format!(" #anchor block-{i}"));
    }
    p.push('\n');
    p
}

fn gen_body(rng: &mut Rng, cov: &mut Coverage) -> String {
    if rng.chance(10) {
        cov.empty_body = true;
        return String::new();
    }
    let mut body = String::new();
    let blocks = 1 + rng.below(6);
    for i in 0..blocks {
        match rng.below(10) {
            0 => {
                cov.heading = true;
                let n = 2 + rng.below(3);
                body.push_str(&format!("# {}\n", words(rng, n, cov)));
            }
            1 => {
                cov.list = true;
                cov.cite = true;
                body.push_str(&format!(
                    "- {}\n- {} [@{}]\n",
                    words(rng, 3, cov),
                    words(rng, 2, cov),
                    rng.pick(&CITE_KEYS)
                ));
            }
            2 => {
                cov.code_block = true;
                body.push_str("```rust\nlet x = 1; // not counted, not cited\n```\n");
            }
            _ => body.push_str(&gen_paragraph(rng, i, cov)),
        }
        body.push('\n');
    }
    body
}

fn quoted(s: impl AsRef<str>) -> String {
    format!("\"{}\"", s.as_ref().replace('"', ""))
}

/// Generate one note source. `i` keeps ids unique across the corpus so the
/// `notes` primary key never collides within the shared database.
fn gen_note(rng: &mut Rng, i: usize, cov: &mut Coverage) -> String {
    let mut fm = String::new();
    let kind = rng.below(4);
    let id_line = if rng.chance(85) {
        format!("id: note-{i}\n")
    } else {
        cov.missing_id = true;
        String::new()
    };
    match kind {
        0 => {
            // Plain morg note, sometimes with an id: key (which is recorded
            // as a span but not an identity).
            cov.plain = true;
            if rng.chance(50) {
                cov.plain_with_id = true;
                fm.push_str(&format!("id: note-{i}\n"));
            }
            if rng.chance(50) {
                fm.push_str("tags: [inbox]\n");
            }
        }
        1 => {
            cov.source = true;
            fm.push_str(&id_line);
            fm.push_str("kind: source\n");
            if rng.chance(60) {
                fm.push_str("status: read\n");
            }
        }
        2 => {
            cov.topic = true;
            fm.push_str(&id_line);
            fm.push_str("kind: topic\n");
            if rng.chance(60) {
                cov.aliases = true;
                fm.push_str(&format!(
                    "aliases: [{}, {}]\n",
                    quoted(rng.pick(&MULTIBYTE_WORDS)),
                    quoted(rng.pick(&ASCII_WORDS)),
                ));
            }
            if rng.chance(60) {
                cov.related = true;
                let mut targets = vec![(*rng.pick(&TARGETS)).to_owned()];
                if rng.chance(50) {
                    cov.related_anchor = true;
                    targets.push("circles#evidence-2".to_owned());
                }
                fm.push_str(&format!("related: [{}]\n", targets.join(", ")));
            }
        }
        _ => {
            cov.argument = true;
            fm.push_str(&id_line);
            fm.push_str("kind: argument\n");
            if rng.chance(50) {
                cov.question_form = true;
                fm.push_str("form: question\n");
            } else {
                fm.push_str("form: claim\n");
            }
            if rng.chance(70) {
                cov.thesis = true;
                fm.push_str(&format!("thesis: {}\n", quoted(rng.pick(&MULTIBYTE_WORDS))));
            }
            if rng.chance(85) {
                cov.relations = true;
                fm.push_str("relations:\n");
                for _ in 0..1 + rng.below(3) {
                    let target = if rng.chance(40) {
                        cov.relation_anchor = true;
                        "specific-leaf-area#evidence-1"
                    } else {
                        rng.pick(&TARGETS)
                    };
                    let stance_i = rng.below(3);
                    cov.every_stance[stance_i] = true;
                    let stance = STANCES[stance_i];
                    if rng.chance(75) {
                        let op_i = rng.below(6);
                        cov.every_op[op_i] = true;
                        fm.push_str(&format!(
                            "  - {{ to: {target}, stance: {stance}, op: {} }}\n",
                            OPS[op_i]
                        ));
                    } else {
                        cov.relation_without_op = true;
                        fm.push_str(&format!("  - {{ to: {target}, stance: {stance} }}\n"));
                    }
                }
            }
            if rng.chance(50) {
                fm.push_str("status: draft\n");
            }
        }
    }
    if kind != 0 {
        if rng.chance(70) {
            cov.multibyte = true;
            fm.push_str(&format!("title: {}\n", quoted(rng.pick(&MULTIBYTE_WORDS))));
        }
        if rng.chance(50) {
            cov.uuid = true;
            fm.push_str(&format!(
                "uuid: 0192f3c4-7a11-7bbb-8f55-{:012x}\n",
                i as u64
            ));
        }
        if rng.chance(30) {
            cov.publish = true;
            fm.push_str("publish: true\n");
        }
        if rng.chance(40) {
            cov.created = true;
            fm.push_str(&format!("created: 2026-{:02}-01\n", 1 + rng.below(12)));
        }
        if rng.chance(30) {
            cov.extra = true;
            fm.push_str(&format!(
                "extra:\n  layout: {}\n  depth: {}\n  draft: true\n  tags: [{}]\n  nested:\n    key: {}\n",
                quoted(rng.pick(&MULTIBYTE_WORDS)),
                rng.below(9),
                quoted(rng.pick(&ASCII_WORDS)),
                quoted(rng.pick(&ASCII_WORDS)),
            ));
        }
    }
    format!("---\n{fm}---\n\n{}", gen_body(rng, cov))
}

// ---------------------------------------------------------------------------
// The property
// ---------------------------------------------------------------------------

fn extract(source: &str) -> FileFacts {
    let parsed = parse_document(source);
    morog_sema::analyze_document(&parsed.document).facts
}

/// Store `facts` as a fresh file row and load them back.
fn store_then_load(conn: &Connection, path: &str, source: &str, facts: &FileFacts) -> FileFacts {
    conn.execute(
        "INSERT INTO files (path, mtime_ns, size, hash) VALUES (?1, 0, ?2, x'00')",
        rusqlite::params![path, source.len() as i64],
    )
    .unwrap();
    let file_id = conn.last_insert_rowid();
    store_file_facts(conn, file_id, source, facts).unwrap();
    load_file_facts(conn, file_id).unwrap()
}

#[test]
fn facts_round_trip_generated_corpus() {
    let conn = Connection::open_in_memory().unwrap();
    schema::create_schema(&conn).unwrap();

    let mut rng = Rng::new(0x5EED_0A57);
    let mut cov = Coverage::default();
    for i in 0..500 {
        let source = gen_note(&mut rng, i, &mut cov);
        let facts = extract(&source);
        let loaded = store_then_load(&conn, &format!("gen/note-{i}.md"), &source, &facts);
        assert_eq!(
            facts, loaded,
            "round-trip mismatch for generated note {i}:\n{source}"
        );
    }
    cov.assert_complete();
}

/// Hand-written edge cases that the generator covers only probabilistically
/// (or not at all), pinned so they never rot out of the corpus.
#[test]
fn facts_round_trip_edge_cases() {
    let cases: &[&str] = &[
        // Empty file, frontmatter-free file, body-free frontmatter.
        "",
        "Just a paragraph, no frontmatter.\n",
        "---\nkind: source\nid: s1\n---\n",
        // Plain note with an id: key — id_span is recorded even without kind.
        "---\nid: stray\n---\n\nBody. #mine\n",
        // Kinded note with a missing id (error path still extracts).
        "---\nkind: topic\n---\n\nAn id-less topic [@k].\n",
        // Unknown kind degrades to plain.
        "---\nkind: journal\nid: j1\n---\n\nText.\n",
        // Raw target with a trailing `#` (anchor splits to None; `to` keeps
        // the raw text).
        "---\nkind: argument\nid: a-edge\nform: claim\nrelations:\n  - { to: x#, stance: supports }\n---\n\nBody.\n",
        // Multi-# target: anchor is everything after the first `#`.
        "---\nkind: argument\nid: a-multi\nform: claim\nrelations:\n  - { to: a#b#c, stance: opposes, op: create }\n---\n\nBody.\n",
        // Multi-byte everything: title, alias, thesis-free body, locator.
        "---\nkind: topic\nid: unicode-topic\ntitle: \"日本語 🦀 café\"\naliases: [\"ß\", \"中文\"]\nrelated: [café-topic]\n---\n\n統計 ανάλυση [@key, σ. 42] #anchor evidence-1\n\n自分の考え。 #mine\n",
        // Duplicate anchors within a file (first wins per block scan).
        "---\nkind: topic\nid: dup-anchors\n---\n\nOne [@a]. #anchor same\n\nTwo [@b]. #anchor same\n",
        // Every block shape in one note.
        "---\nkind: topic\nid: shapes\n---\n\n# Heading\n\nPara [@k, p. 1] and [link](id:t#a). #anchor p1\n\n- item one [@k2]\n- item two #mine\n\n```rust\nfn ignored() {}\n```\n\n> callout-ish quote block\n",
    ];

    let conn = Connection::open_in_memory().unwrap();
    schema::create_schema(&conn).unwrap();
    for (i, source) in cases.iter().enumerate() {
        let facts = extract(source);
        let loaded = store_then_load(&conn, &format!("edge/case-{i}.md"), source, &facts);
        assert_eq!(
            facts, loaded,
            "round-trip mismatch for edge case {i}:\n{source}"
        );
    }
}

/// The fixture-vault notes (committed under crates/morog/tests/fixtures)
/// round-trip too — real files, not generated ones.
#[test]
fn facts_round_trip_fixture_vault() {
    let vault = concat!(env!("CARGO_MANIFEST_DIR"), "/../morog/tests/fixtures/vault");
    let pubvault = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../morog/tests/fixtures/pubvault"
    );

    let mut checked = 0;
    let mut stack = vec![
        std::path::PathBuf::from(vault),
        std::path::PathBuf::from(pubvault),
    ];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "md") {
                // One database per file: the vault deliberately contains a
                // duplicate note id, which cannot round-trip in a shared db
                // (the second `notes` row is dropped; `sta::id` lints it).
                let conn = Connection::open_in_memory().unwrap();
                schema::create_schema(&conn).unwrap();
                let source = std::fs::read_to_string(&path).unwrap();
                let facts = extract(&source);
                let loaded = store_then_load(&conn, &path.display().to_string(), &source, &facts);
                assert_eq!(facts, loaded, "round-trip mismatch for {}", path.display());
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 14, "expected the 14 committed fixture notes");
}

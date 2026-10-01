//! `morog graph` — DOT export of the discourse graph (plan §7.1, M6).
//!
//! The exporter reads the resolved edges (`v_edges`, plus `relations` for
//! anchor-level targets and `v_evidence` under `--evidence`) from an
//! **existing** index, builds a `petgraph::StableGraph`, applies the node
//! and edge filters, and emits DOT. Dangling relations (`v_dangling`) never
//! enter the graph; their count goes to stderr as a notice.
//!
//! DOT is emitted by hand rather than through `graphviz-rust`'s builder:
//! that builder stores identifiers verbatim (`Id::Escaped` expects the
//! caller to deliver an already-quoted string), so the one correctness-
//! critical part — escaping — would be hand-written either way, while the
//! crate drags a whole DOT *parser* (pest) into the tree for a pure-emission
//! job. A quoted-string emitter in this module keeps the dependency small
//! and the escaping testable, matching the hand-rolled `json.rs`.
//!
//! Rendering, per the plan §7.1 table: sources `shape=note`, topics
//! `shape=ellipse`, arguments `shape=hexagon` filled by Bloom depth
//! (light→dark, 6 steps); `supports` green solid, `opposes` red dashed,
//! `qualifies` orange dotted, `cites` grey; the op is the edge label.
//! `rankdir=BT` stacks sources below topics below arguments under `dot`;
//! use `sfdp` for whole-vault renders.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use morog_index::{QueryOutput, Value};
use morog_sema::{BloomOp, NoteKind, Stance};
use petgraph::stable_graph::{NodeIndex, StableDiGraph};

use crate::cli::GraphArgs;

/// Bloom-depth fill shades for argument hexagons, ordinal 0 (`recall`,
/// lightest) through 5 (`create`, darkest): the ColorBrewer six-class Blues
/// sequential scheme, which stays distinguishable printed and laid out with
/// either `dot` or `sfdp`.
const BLOOM_FILL: [&str; 6] = [
    "#eff3ff", "#c6dbef", "#9ecae1", "#6baed6", "#3182bd", "#08519c",
];
/// From this Bloom ordinal up the fill is dark enough to need white text.
const BLOOM_WHITE_TEXT: u8 = 4;
/// Fill for arguments with no typed relation (no depth row / NULL depth).
const NO_DEPTH_FILL: &str = "#ffffff";

const SUPPORTS_COLOR: &str = "#1a7f37"; // green, solid
const OPPOSES_COLOR: &str = "#cf222e"; // red, dashed
const QUALIFIES_COLOR: &str = "#e36209"; // orange, dotted
const CITES_COLOR: &str = "#808080"; // grey
const NEUTRAL_COLOR: &str = "#999999"; // related / mentions

/// Evidence-block label text is truncated to roughly this many characters.
const EVIDENCE_TEXT_CHARS: usize = 40;

/// A note loaded from the index: one graph node candidate.
#[derive(Debug, Clone)]
struct Note {
    id: String,
    kind: NoteKind,
    title: Option<String>,
    publish: bool,
    /// Bloom depth from `v_argument_depth` (arguments only).
    depth: Option<u8>,
}

/// One resolved relation (a `v_edges` row, plus the target anchor that only
/// the `relations` table carries, needed for `--evidence`).
#[derive(Debug, Clone)]
struct EdgeRow {
    src: String,
    dst: String,
    rel: Rel,
}

/// The edge payload stored in the petgraph graph.
#[derive(Debug, Clone)]
struct Rel {
    kind: String,
    stance: Option<Stance>,
    op: Option<BloomOp>,
    dst_anchor: Option<String>,
}

/// One evidence block of a topic (grouped `v_evidence` rows).
#[derive(Debug, Clone)]
struct EvidenceBlock {
    topic: String,
    idx: i64,
    anchor: Option<String>,
    citekeys: Vec<String>,
    text: String,
}

/// The parsed filter set.
struct Filters {
    kinds: Option<HashSet<NoteKind>>,
    stances: Option<HashSet<Stance>>,
    min_op: Option<BloomOp>,
    published: bool,
}

impl Filters {
    fn keep_note(&self, note: &Note) -> bool {
        self.kinds
            .as_ref()
            .is_none_or(|set| set.contains(&note.kind))
            && (!self.published || note.publish)
    }

    fn keep_edge(&self, rel: &Rel) -> bool {
        self.stances
            .as_ref()
            .is_none_or(|set| rel.stance.is_some_and(|s| set.contains(&s)))
            && self
                .min_op
                .is_none_or(|min| rel.op.is_some_and(|op| op >= min))
    }
}

/// What gets emitted: the filtered graph flattened into render nodes and
/// edges, with evidence blocks expanded into their topic clusters.
#[derive(Debug, Default)]
struct Render {
    nodes: Vec<RenderNode>,
    edges: Vec<RenderEdge>,
}

#[derive(Debug)]
enum RenderKind {
    Note(NoteKind),
    Evidence,
}

#[derive(Debug)]
struct RenderNode {
    id: String,
    label: String,
    kind: RenderKind,
    /// Bloom depth (arguments only).
    depth: Option<u8>,
    /// Topic cluster this node belongs to (`--evidence` only).
    cluster: Option<String>,
}

#[derive(Debug)]
struct RenderEdge {
    src: String,
    dst: String,
    kind: String,
    stance: Option<Stance>,
    op: Option<BloomOp>,
}

pub fn run(args: GraphArgs, format: &str) -> ExitCode {
    match run_inner(args, format) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("morog graph: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: GraphArgs, format: &str) -> Result<(), String> {
    let filters = parse_filters(&args)?;

    let vault = args.vault.unwrap_or_else(|| PathBuf::from("."));
    let vault = vault
        .canonicalize()
        .map_err(|e| format!("cannot open vault {}: {e}", vault.display()))?;
    // Require an existing index; `run_query` reports a missing one with a
    // pointer to `morog index` (never build silently — plan §3.1).
    let db_path = morog_index::cache_db_path(&vault);

    let notes = load_notes(&db_path)?;
    let edges = load_edges(&db_path, args.evidence)?;
    let evidence = if args.evidence {
        load_evidence(&db_path)?
    } else {
        Vec::new()
    };
    let dangling = load_dangling_count(&db_path)?;

    let mut graph = build_graph(notes, edges, &filters);
    if let Some(root) = args.root.as_deref() {
        restrict_to_neighborhood(&mut graph, root, args.depth.unwrap_or(1))?;
    }

    let render = render_graph(&graph, &evidence);
    let text = match format {
        "json" => emit_json(&render),
        _ => emit_dot(&render),
    };

    match &args.output {
        Some(path) => std::fs::write(path, text)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?,
        None => print!("{text}"),
    }

    if dangling > 0 {
        eprintln!(
            "morog graph: skipped {dangling} dangling relation{} (run `morog lint` for details)",
            if dangling == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// Parse `--kinds`, `--stance`, and `--min-op` against the closed sema
/// vocabularies, so the accepted spellings stay defined in one place.
fn parse_filters(args: &GraphArgs) -> Result<Filters, String> {
    let kinds = if args.kinds.is_empty() {
        None
    } else {
        let mut set = HashSet::new();
        for raw in &args.kinds {
            set.insert(NoteKind::parse(raw).ok_or_else(|| {
                format!(
                    "unknown kind `{raw}` (expected {})",
                    NoteKind::ALL.join(", ")
                )
            })?);
        }
        Some(set)
    };
    let stances = if args.stance.is_empty() {
        None
    } else {
        let mut set = HashSet::new();
        for raw in &args.stance {
            set.insert(Stance::parse(raw).ok_or_else(|| {
                format!(
                    "unknown stance `{raw}` (expected {})",
                    Stance::ALL.join(", ")
                )
            })?);
        }
        Some(set)
    };
    let min_op = args
        .min_op
        .as_deref()
        .map(|raw| {
            BloomOp::parse(raw)
                .ok_or_else(|| format!("unknown op `{raw}` (expected {})", BloomOp::ALL.join(", ")))
        })
        .transpose()?;
    Ok(Filters {
        kinds,
        stances,
        min_op,
        published: args.published,
    })
}

// ---------------------------------------------------------------- loading

/// Run one SELECT against the index, mapping errors to CLI messages (a
/// missing index becomes the "run `morog index` first" error).
fn query(db_path: &std::path::Path, sql: &str) -> Result<QueryOutput, String> {
    morog_index::run_query(db_path, sql).map_err(|e| e.to_string())
}

fn text_cell(v: &Value) -> Option<String> {
    match v {
        Value::Text(t) => Some(t.clone()),
        _ => None,
    }
}

fn int_cell(v: &Value) -> Option<i64> {
    match v {
        Value::Integer(n) => Some(*n),
        _ => None,
    }
}

fn load_notes(db_path: &std::path::Path) -> Result<Vec<Note>, String> {
    let out = query(
        db_path,
        "SELECT n.note_id, n.kind, n.title, n.publish, d.depth
         FROM notes n
         LEFT JOIN v_argument_depth d ON d.argument = n.note_id
         ORDER BY n.note_id",
    )?;
    out.rows
        .iter()
        .map(|row| {
            let id = text_cell(&row[0]).ok_or("corrupt index: non-text note_id")?;
            let kind = text_cell(&row[1])
                .as_deref()
                .and_then(NoteKind::parse)
                .ok_or_else(|| format!("corrupt index: bad kind on note `{id}`"))?;
            Ok(Note {
                id,
                kind,
                title: text_cell(&row[2]),
                publish: int_cell(&row[3]) == Some(1),
                depth: int_cell(&row[4]).and_then(|d| u8::try_from(d).ok()),
            })
        })
        .collect()
}

/// Resolved edges. The base view is `v_edges`; under `--evidence` the same
/// rows are read from `relations` instead, which still carries `dst_anchor`
/// (needed to retarget anchor-level relations onto evidence nodes).
fn load_edges(db_path: &std::path::Path, with_anchor: bool) -> Result<Vec<EdgeRow>, String> {
    let sql = if with_anchor {
        "SELECT r.src_note, r.dst, r.kind, r.stance, r.op, r.dst_anchor
         FROM relations r
         JOIN notes n ON n.note_id = r.dst
         WHERE r.src_note IS NOT NULL
         ORDER BY r.src_note, r.dst, r.kind, r.stance, r.op"
    } else {
        "SELECT src, dst, kind, stance, op, NULL AS dst_anchor
         FROM v_edges
         WHERE src IS NOT NULL
         ORDER BY src, dst, kind, stance, op"
    };
    let out = query(db_path, sql)?;
    out.rows
        .iter()
        .map(|row| {
            let src = text_cell(&row[0]).ok_or("corrupt index: non-text edge src")?;
            let dst = text_cell(&row[1]).ok_or("corrupt index: non-text edge dst")?;
            let kind = text_cell(&row[2]).ok_or("corrupt index: non-text edge kind")?;
            Ok(EdgeRow {
                src,
                dst,
                rel: Rel {
                    kind,
                    stance: text_cell(&row[3]).as_deref().and_then(Stance::parse),
                    op: int_cell(&row[4])
                        .and_then(|n| u8::try_from(n).ok())
                        .and_then(bloom_from_ordinal),
                    dst_anchor: text_cell(&row[5]),
                },
            })
        })
        .collect()
}

/// `v_evidence` rows grouped into one [`EvidenceBlock`] per (topic, idx),
/// collecting the carried citekeys.
fn load_evidence(db_path: &std::path::Path) -> Result<Vec<EvidenceBlock>, String> {
    let out = query(
        db_path,
        "SELECT topic, idx, anchor, citekey, text
         FROM v_evidence
         ORDER BY topic, idx, citekey",
    )?;
    let mut blocks: Vec<EvidenceBlock> = Vec::new();
    for row in &out.rows {
        let topic = text_cell(&row[0]).ok_or("corrupt index: non-text evidence topic")?;
        let idx = int_cell(&row[1]).ok_or("corrupt index: non-integer evidence idx")?;
        let citekey = text_cell(&row[3]);
        match blocks.last_mut() {
            Some(last) if last.topic == topic && last.idx == idx => {
                last.citekeys.extend(citekey);
            }
            _ => blocks.push(EvidenceBlock {
                topic,
                idx,
                anchor: text_cell(&row[2]),
                citekeys: citekey.into_iter().collect(),
                text: text_cell(&row[4]).unwrap_or_default(),
            }),
        }
    }
    Ok(blocks)
}

/// How many relations stay out of the graph entirely: unresolved note
/// targets and unresolved citekeys. (`unknown-anchor` rows still resolve at
/// the note level and are drawn, so they are not counted.)
fn load_dangling_count(db_path: &std::path::Path) -> Result<i64, String> {
    let out = query(
        db_path,
        "SELECT COUNT(*) FROM v_dangling
         WHERE reason IN ('unknown-note', 'unknown-citekey')",
    )?;
    Ok(out.rows.first().and_then(|r| int_cell(&r[0])).unwrap_or(0))
}

fn bloom_from_ordinal(n: u8) -> Option<BloomOp> {
    BloomOp::ALL.get(n as usize).and_then(|s| BloomOp::parse(s))
}

// --------------------------------------------------------- graph building

/// Insert the notes that pass the node filters and the edges that pass the
/// edge filters (an edge also needs both endpoints present).
fn build_graph(
    notes: Vec<Note>,
    edges: Vec<EdgeRow>,
    filters: &Filters,
) -> StableDiGraph<Note, Rel> {
    let mut graph = StableDiGraph::new();
    let mut index: HashMap<String, NodeIndex> = HashMap::new();
    for note in notes {
        if filters.keep_note(&note) {
            let id = note.id.clone();
            let ix = graph.add_node(note);
            index.insert(id, ix);
        }
    }
    for edge in edges {
        if !filters.keep_edge(&edge.rel) {
            continue;
        }
        if let (Some(&src), Some(&dst)) = (index.get(&edge.src), index.get(&edge.dst)) {
            graph.add_edge(src, dst, edge.rel);
        }
    }
    graph
}

/// Keep only the nodes within `depth` hops of `root`, following edges in
/// both directions (plan §7.1's `--root ID --depth N`).
fn restrict_to_neighborhood(
    graph: &mut StableDiGraph<Note, Rel>,
    root: &str,
    depth: usize,
) -> Result<(), String> {
    let root_ix = graph
        .node_indices()
        .find(|&ix| graph[ix].id == root)
        .ok_or_else(|| {
            format!("root `{root}` is not in the graph (unknown id, or excluded by filters)")
        })?;
    let mut keep: HashSet<NodeIndex> = HashSet::from([root_ix]);
    let mut frontier = VecDeque::from([(root_ix, 0usize)]);
    while let Some((ix, hops)) = frontier.pop_front() {
        if hops == depth {
            continue;
        }
        for next in graph.neighbors_undirected(ix) {
            if keep.insert(next) {
                frontier.push_back((next, hops + 1));
            }
        }
    }
    graph.retain_nodes(|_, ix| keep.contains(&ix));
    Ok(())
}

// -------------------------------------------------------------- rendering

/// Flatten the graph for emission. With evidence blocks present, each topic
/// that has them becomes a cluster: the topic node plus one node per
/// evidence block, relation edges onto a known `topic#anchor` retargeted to
/// the block node, and grey cite edges from blocks to their source notes
/// (when the citekey is a source note in the graph).
fn render_graph(graph: &StableDiGraph<Note, Rel>, evidence: &[EvidenceBlock]) -> Render {
    let present: HashMap<&str, &Note> = graph
        .node_indices()
        .map(|ix| (graph[ix].id.as_str(), &graph[ix]))
        .collect();
    let clustered_topics: HashSet<&str> = evidence.iter().map(|b| b.topic.as_str()).collect();
    // Evidence nodes, keyed by (topic, anchor) for edge retargeting.
    let mut anchor_nodes: HashMap<(&str, &str), String> = HashMap::new();
    let mut render = Render::default();

    let mut node_indices: Vec<NodeIndex> = graph.node_indices().collect();
    node_indices.sort_by(|&a, &b| graph[a].id.cmp(&graph[b].id));
    for &ix in &node_indices {
        let note = &graph[ix];
        let in_cluster =
            note.kind == NoteKind::Topic && clustered_topics.contains(note.id.as_str());
        render.nodes.push(RenderNode {
            id: note.id.clone(),
            label: note.title.clone().unwrap_or_else(|| note.id.clone()),
            kind: RenderKind::Note(note.kind),
            depth: note.depth,
            cluster: in_cluster.then(|| note.id.clone()),
        });
    }

    for block in evidence {
        let Some(topic) = present.get(block.topic.as_str()) else {
            continue; // topic filtered out of the graph
        };
        let name = block
            .anchor
            .clone()
            .unwrap_or_else(|| format!("block {}", block.idx));
        let id = format!("{}#{name}", block.topic);
        if let Some(anchor) = &block.anchor {
            anchor_nodes.insert((topic.id.as_str(), anchor.as_str()), id.clone());
        }
        render.nodes.push(RenderNode {
            id: id.clone(),
            label: format!(
                "{name}\n{}",
                truncate_chars(&block.text, EVIDENCE_TEXT_CHARS)
            ),
            kind: RenderKind::Evidence,
            depth: None,
            cluster: Some(block.topic.clone()),
        });
        for citekey in &block.citekeys {
            if present
                .get(citekey.as_str())
                .is_some_and(|n| n.kind == NoteKind::Source)
            {
                render.edges.push(RenderEdge {
                    src: id.clone(),
                    dst: citekey.clone(),
                    kind: "cites".into(),
                    stance: None,
                    op: None,
                });
            }
        }
    }

    let mut edge_indices: Vec<_> = graph.edge_indices().collect();
    edge_indices.sort();
    for ex in edge_indices {
        let (src_ix, dst_ix) = graph.edge_endpoints(ex).expect("edge in graph");
        let rel = &graph[ex];
        let dst_note = &graph[dst_ix];
        // An `id#anchor` target lands on the evidence node when it exists.
        let dst = rel
            .dst_anchor
            .as_deref()
            .and_then(|anchor| anchor_nodes.get(&(dst_note.id.as_str(), anchor)))
            .cloned()
            .unwrap_or_else(|| dst_note.id.clone());
        render.edges.push(RenderEdge {
            src: graph[src_ix].id.clone(),
            dst,
            kind: rel.kind.clone(),
            stance: rel.stance,
            op: rel.op,
        });
    }
    render
}

/// Truncate to at most `max` characters, marking the cut with an ellipsis.
fn truncate_chars(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((byte, _)) => format!("{}…", s[..byte].trim_end()),
        None => s.to_string(),
    }
}

// --------------------------------------------------------------- emission

/// A DOT double-quoted string: `"` and `\` escaped, newlines turned into
/// the `\n` (centered line break) escape.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn node_attrs(node: &RenderNode) -> String {
    let label = quoted(&node.label);
    match node.kind {
        RenderKind::Note(NoteKind::Source) => format!("label={label}, shape=note"),
        RenderKind::Note(NoteKind::Topic) => format!("label={label}, shape=ellipse"),
        RenderKind::Note(NoteKind::Argument) => {
            let (fill, font) = match node.depth {
                Some(d) => {
                    let fill = BLOOM_FILL[usize::from(d.min(5))];
                    let font = if d >= BLOOM_WHITE_TEXT {
                        ", fontcolor=\"#ffffff\""
                    } else {
                        ""
                    };
                    (fill, font)
                }
                None => (NO_DEPTH_FILL, ""),
            };
            format!("label={label}, shape=hexagon, style=filled, fillcolor=\"{fill}\"{font}")
        }
        RenderKind::Evidence => format!("label={label}, shape=box, fontsize=10"),
    }
}

fn edge_attrs(edge: &RenderEdge) -> String {
    let (color, style) = match edge.stance {
        Some(Stance::Supports) => (SUPPORTS_COLOR, "solid"),
        Some(Stance::Opposes) => (OPPOSES_COLOR, "dashed"),
        Some(Stance::Qualifies) => (QUALIFIES_COLOR, "dotted"),
        None if edge.kind == "cites" => (CITES_COLOR, "solid"),
        None => (NEUTRAL_COLOR, "solid"),
    };
    let mut attrs = format!("color=\"{color}\", style={style}");
    if let Some(op) = edge.op {
        let _ = write!(attrs, ", label={}", quoted(op.as_str()));
    }
    attrs
}

fn emit_dot(render: &Render) -> String {
    let mut out = String::new();
    out.push_str("digraph morog {\n");
    out.push_str("  rankdir=BT;\n");
    out.push_str("  node [fontname=\"Helvetica\"];\n");
    out.push_str("  edge [fontname=\"Helvetica\", fontsize=10];\n");

    // Free-standing nodes first, then one subgraph per evidence cluster
    // (grouped in a single pass; the node list is sorted by id, so cluster
    // members stay in a stable order).
    let mut clusters: HashMap<&str, Vec<&RenderNode>> = HashMap::new();
    for node in &render.nodes {
        match node.cluster.as_deref() {
            None => {
                let _ = writeln!(out, "  {} [{}];", quoted(&node.id), node_attrs(node));
            }
            Some(cluster) => clusters.entry(cluster).or_default().push(node),
        }
    }
    let mut cluster_ids: Vec<&str> = clusters.keys().copied().collect();
    cluster_ids.sort_unstable();
    for cluster in cluster_ids {
        let members = &clusters[cluster];
        // The cluster is labelled with its topic's label (title or id).
        let label = members
            .iter()
            .find(|n| n.id == cluster)
            .map_or(cluster, |n| n.label.as_str());
        let _ = writeln!(
            out,
            "  subgraph {} {{",
            quoted(&format!("cluster_{cluster}"))
        );
        let _ = writeln!(out, "    label={};", quoted(label));
        out.push_str("    color=\"#bbbbbb\";\n");
        for node in members {
            let _ = writeln!(out, "    {} [{}];", quoted(&node.id), node_attrs(node));
        }
        out.push_str("  }\n");
    }
    for edge in &render.edges {
        let _ = writeln!(
            out,
            "  {} -> {} [{}];",
            quoted(&edge.src),
            quoted(&edge.dst),
            edge_attrs(edge)
        );
    }
    out.push_str("}\n");
    out
}

/// `--format json`: a plain node/edge list mirroring the DOT content.
fn emit_json(render: &Render) -> String {
    let mut out = String::from("{\n  \"nodes\": [");
    for (i, node) in render.nodes.iter().enumerate() {
        let kind = match node.kind {
            RenderKind::Note(k) => k.as_str(),
            RenderKind::Evidence => "evidence",
        };
        let _ = write!(
            out,
            "{}\n    {{\"id\": \"{}\", \"kind\": \"{kind}\", \"label\": \"{}\", \"depth\": {}, \"cluster\": {}}}",
            if i > 0 { "," } else { "" },
            crate::json::escape(&node.id),
            crate::json::escape(&node.label),
            node.depth.map_or("null".into(), |d| d.to_string()),
            node.cluster
                .as_ref()
                .map_or("null".into(), |c| format!("\"{}\"", crate::json::escape(c))),
        );
    }
    out.push_str("\n  ],\n  \"edges\": [");
    for (i, edge) in render.edges.iter().enumerate() {
        let _ = write!(
            out,
            "{}\n    {{\"src\": \"{}\", \"dst\": \"{}\", \"kind\": \"{}\", \"stance\": {}, \"op\": {}}}",
            if i > 0 { "," } else { "" },
            crate::json::escape(&edge.src),
            crate::json::escape(&edge.dst),
            crate::json::escape(&edge.kind),
            edge.stance
                .map_or("null".into(), |s| format!("\"{}\"", s.as_str())),
            edge.op
                .map_or("null".into(), |o| format!("\"{}\"", o.as_str())),
        );
    }
    out.push_str("\n  ]\n}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, kind: NoteKind) -> Note {
        Note {
            id: id.into(),
            kind,
            title: None,
            publish: false,
            depth: None,
        }
    }

    fn edge(src: &str, dst: &str, stance: Option<Stance>, op: Option<BloomOp>) -> EdgeRow {
        EdgeRow {
            src: src.into(),
            dst: dst.into(),
            rel: Rel {
                kind: "argues".into(),
                stance,
                op,
                dst_anchor: None,
            },
        }
    }

    fn no_filters() -> Filters {
        Filters {
            kinds: None,
            stances: None,
            min_op: None,
            published: false,
        }
    }

    fn ids(graph: &StableDiGraph<Note, Rel>) -> Vec<String> {
        let mut ids: Vec<String> = graph
            .node_indices()
            .map(|ix| graph[ix].id.clone())
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn dot_quoting_escapes_quotes_backslashes_and_newlines() {
        assert_eq!(quoted("plain-id"), "\"plain-id\"");
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\b"), "\"a\\\\b\"");
        assert_eq!(quoted("two\nlines\r"), "\"two\\nlines\"");
    }

    #[test]
    fn truncation_is_char_boundary_safe() {
        assert_eq!(truncate_chars("short", 40), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        // Multibyte: must cut between chars, not bytes.
        assert_eq!(truncate_chars("ééééé", 2), "éé…");
        assert_eq!(truncate_chars("ab   cdef", 4), "ab…");
    }

    #[test]
    fn kind_and_published_filters_drop_nodes_and_their_edges() {
        let notes = vec![
            note("s", NoteKind::Source),
            note("t", NoteKind::Topic),
            note("a", NoteKind::Argument),
        ];
        let edges = vec![edge("a", "t", Some(Stance::Supports), None)];
        let filters = Filters {
            kinds: Some(HashSet::from([NoteKind::Topic, NoteKind::Argument])),
            ..no_filters()
        };
        let graph = build_graph(notes.clone(), edges.clone(), &filters);
        assert_eq!(ids(&graph), ["a", "t"]);
        assert_eq!(graph.edge_count(), 1);

        // `--published` with no published notes empties the graph.
        let filters = Filters {
            published: true,
            ..no_filters()
        };
        let graph = build_graph(notes, edges, &filters);
        assert!(ids(&graph).is_empty());
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn stance_and_min_op_filter_edges_only() {
        let notes = vec![
            note("a", NoteKind::Argument),
            note("t", NoteKind::Topic),
            note("u", NoteKind::Topic),
        ];
        let edges = vec![
            edge("a", "t", Some(Stance::Supports), Some(BloomOp::Analyze)),
            edge("a", "u", Some(Stance::Opposes), Some(BloomOp::Recall)),
            edge("a", "u", None, None), // unstanced, un-opped
        ];
        let filters = Filters {
            stances: Some(HashSet::from([Stance::Supports])),
            ..no_filters()
        };
        let graph = build_graph(notes.clone(), edges.clone(), &filters);
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(ids(&graph), ["a", "t", "u"]); // nodes untouched

        let filters = Filters {
            min_op: Some(BloomOp::Analyze),
            ..no_filters()
        };
        let graph = build_graph(notes, edges, &filters);
        assert_eq!(graph.edge_count(), 1); // recall and op-less edges dropped
    }

    #[test]
    fn neighborhood_follows_both_directions_to_depth() {
        // chain: w -> x -> y -> z, rooted at x.
        let notes = vec![
            note("w", NoteKind::Argument),
            note("x", NoteKind::Topic),
            note("y", NoteKind::Topic),
            note("z", NoteKind::Source),
        ];
        let edges = vec![
            edge("w", "x", None, None),
            edge("x", "y", None, None),
            edge("y", "z", None, None),
        ];
        let mut graph = build_graph(notes.clone(), edges.clone(), &no_filters());
        restrict_to_neighborhood(&mut graph, "x", 1).unwrap();
        assert_eq!(ids(&graph), ["w", "x", "y"]); // both directions, 1 hop

        let mut graph = build_graph(notes, edges, &no_filters());
        restrict_to_neighborhood(&mut graph, "x", 2).unwrap();
        assert_eq!(ids(&graph), ["w", "x", "y", "z"]);

        let err = restrict_to_neighborhood(&mut graph, "nope", 1).unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn evidence_expansion_retargets_anchored_edges_and_cites_sources() {
        let notes = vec![
            note("topic", NoteKind::Topic),
            note("arg", NoteKind::Argument),
            note("paper_2020", NoteKind::Source),
        ];
        let edges = vec![EdgeRow {
            src: "arg".into(),
            dst: "topic".into(),
            rel: Rel {
                kind: "argues".into(),
                stance: Some(Stance::Supports),
                op: Some(BloomOp::Analyze),
                dst_anchor: Some("ev-1".into()),
            },
        }];
        let evidence = vec![
            EvidenceBlock {
                topic: "topic".into(),
                idx: 0,
                anchor: Some("ev-1".into()),
                citekeys: vec!["paper_2020".into(), "unknown_key".into()],
                text: "block text".into(),
            },
            EvidenceBlock {
                topic: "topic".into(),
                idx: 1,
                anchor: None,
                citekeys: vec![],
                text: "anchorless".into(),
            },
        ];
        let graph = build_graph(notes, edges, &no_filters());
        let render = render_graph(&graph, &evidence);

        let dot = emit_dot(&render);
        assert!(dot.contains("subgraph \"cluster_topic\""), "{dot}");
        assert!(dot.contains("\"topic#ev-1\""), "{dot}");
        assert!(dot.contains("\"topic#block 1\""), "{dot}");
        // The anchored relation lands on the evidence node, not the topic.
        assert!(dot.contains("\"arg\" -> \"topic#ev-1\""), "{dot}");
        // Cites edge only to citekeys that are source notes in the graph.
        assert!(dot.contains("\"topic#ev-1\" -> \"paper_2020\""), "{dot}");
        assert!(!dot.contains("unknown_key"), "{dot}");
    }

    #[test]
    fn dot_emission_renders_the_plan_table() {
        let mut arg = note("arg", NoteKind::Argument);
        arg.depth = Some(4); // evaluate: dark fill, white text
        let notes = vec![
            note("src", NoteKind::Source),
            note("top", NoteKind::Topic),
            arg,
        ];
        let edges = vec![
            edge("arg", "top", Some(Stance::Opposes), Some(BloomOp::Evaluate)),
            EdgeRow {
                src: "top".into(),
                dst: "src".into(),
                rel: Rel {
                    kind: "cites".into(),
                    stance: None,
                    op: None,
                    dst_anchor: None,
                },
            },
        ];
        let graph = build_graph(notes, edges, &no_filters());
        let dot = emit_dot(&render_graph(&graph, &[]));

        assert!(dot.contains("rankdir=BT"), "{dot}");
        assert!(dot.contains("shape=note"), "{dot}");
        assert!(dot.contains("shape=ellipse"), "{dot}");
        assert!(dot.contains("shape=hexagon"), "{dot}");
        assert!(dot.contains(BLOOM_FILL[4]), "{dot}");
        assert!(dot.contains("fontcolor=\"#ffffff\""), "{dot}");
        assert!(
            dot.contains(OPPOSES_COLOR) && dot.contains("style=dashed"),
            "{dot}"
        );
        assert!(dot.contains("label=\"evaluate\""), "{dot}");
        assert!(dot.contains(CITES_COLOR), "{dot}");
    }
}

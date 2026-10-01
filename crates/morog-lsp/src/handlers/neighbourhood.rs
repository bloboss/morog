//! Custom `morog/neighbourhood` request (plan §5.6): the JSON subgraph
//! within N hops of a note — the DOT exporter's data shape (nodes carry
//! `id`/`kind`/`title`, edges carry `src`/`dst`/`kind`/`stance`/`op`) —
//! computed from the Analysis xref, **not** SQL, so it reflects open
//! buffers immediately.
//!
//! Like the DOT exporter, dangling references never enter the graph, and
//! hops follow edges in both directions.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use lsp_server::ErrorCode;
use morog_sema::RelationKind;
use serde::Deserialize;
use serde_json::json;

use crate::handlers::HandlerError;
use crate::state::Snapshot;

#[derive(Debug, Deserialize)]
pub(crate) struct NeighbourhoodParams {
    /// The root note id; alternatively derived from `uri`.
    #[serde(default)]
    pub id: Option<String>,
    /// The document whose note is the root, when `id` is absent.
    #[serde(default)]
    pub uri: Option<lsp_types::Url>,
    /// How many hops out from the root (default 1).
    #[serde(default = "default_hops")]
    pub hops: u32,
}

fn default_hops() -> u32 {
    1
}

/// One resolved edge of the discourse graph.
struct Edge {
    src: String,
    dst: String,
    kind: &'static str,
    stance: Option<&'static str>,
    op: Option<&'static str>,
}

pub(crate) fn handle(
    snapshot: &Snapshot,
    params: NeighbourhoodParams,
) -> Result<serde_json::Value, HandlerError> {
    let root = root_id(snapshot, &params)
        .ok_or((ErrorCode::InvalidParams, "no note id to root at".to_owned()))?;
    if snapshot.analysis.resolve_id(&root).is_none() {
        return Err((
            ErrorCode::InvalidParams,
            format!("`{root}` does not resolve to any note"),
        ));
    }

    // Resolved edges only: both endpoints must be declared notes (dangling
    // references never enter the graph, as in the DOT exporter).
    let mut edges: Vec<Edge> = Vec::new();
    for (target, refs) in &snapshot.analysis.xref.backrefs {
        if snapshot.analysis.resolve_id(target).is_none() {
            continue;
        }
        for r in refs {
            let Some(src) = snapshot
                .analysis
                .file(r.file)
                .and_then(|fa| fa.facts.note.id.as_ref())
                .map(|id| id.as_str().to_owned())
            else {
                continue;
            };
            let (kind, stance, op) = match r.kind {
                RelationKind::Argues { stance, op } => {
                    ("argues", Some(stance.as_str()), op.map(|op| op.as_str()))
                }
                RelationKind::Related => ("related", None, None),
                RelationKind::Mentions => ("mentions", None, None),
                RelationKind::Cites => ("cites", None, None),
            };
            edges.push(Edge {
                src,
                dst: target.to_string(),
                kind,
                stance,
                op,
            });
        }
    }

    // Undirected BFS to `hops`.
    let mut adjacency: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for edge in &edges {
        adjacency.entry(&edge.src).or_default().insert(&edge.dst);
        adjacency.entry(&edge.dst).or_default().insert(&edge.src);
    }
    let mut keep: BTreeSet<&str> = BTreeSet::from([root.as_str()]);
    let mut frontier = VecDeque::from([(root.as_str(), 0u32)]);
    while let Some((id, dist)) = frontier.pop_front() {
        if dist == params.hops {
            continue;
        }
        for &next in adjacency.get(id).into_iter().flatten() {
            if keep.insert(next) {
                frontier.push_back((next, dist + 1));
            }
        }
    }

    let nodes: Vec<serde_json::Value> = keep
        .iter()
        .map(|id| {
            let note = snapshot
                .analysis
                .resolve_id(id)
                .and_then(|file| snapshot.analysis.file(file))
                .map(|fa| fa.facts.note.clone());
            json!({
                "id": id,
                "kind": note.as_ref().and_then(|n| n.kind).map(|k| k.as_str()),
                "title": note.as_ref().and_then(|n| n.title.clone()),
            })
        })
        .collect();

    let mut kept_edges: Vec<&Edge> = edges
        .iter()
        .filter(|edge| keep.contains(edge.src.as_str()) && keep.contains(edge.dst.as_str()))
        .collect();
    kept_edges.sort_by(|a, b| {
        (&a.src, &a.dst, a.kind, a.stance, a.op).cmp(&(&b.src, &b.dst, b.kind, b.stance, b.op))
    });
    let edges: Vec<serde_json::Value> = kept_edges
        .iter()
        .map(|edge| {
            json!({
                "src": edge.src,
                "dst": edge.dst,
                "kind": edge.kind,
                "stance": edge.stance,
                "op": edge.op,
            })
        })
        .collect();

    Ok(json!({
        "root": root,
        "hops": params.hops,
        "nodes": nodes,
        "edges": edges,
    }))
}

/// The root id: explicit, or the id declared by the document at `uri`.
fn root_id(snapshot: &Snapshot, params: &NeighbourhoodParams) -> Option<String> {
    if let Some(id) = &params.id {
        return Some(id.clone());
    }
    let path = crate::from_proto::abs_path(params.uri.as_ref()?)?;
    let file = snapshot.analysis.file_id(&path)?;
    snapshot
        .analysis
        .file(file)?
        .facts
        .note
        .id
        .as_ref()
        .map(|id| id.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use expect_test::{Expect, expect};

    use super::*;
    use crate::handlers::test_support::snapshot;

    const SLA: &str = "---\nid: sla\nkind: topic\ntitle: \"SLA\"\nrelated: [lmr]\n---\n\nEvidence [@wright2004]. #anchor e1\n";
    const LMR: &str =
        "---\nid: lmr\nkind: topic\ntitle: \"LMR\"\n---\n\nSee [sla](id:sla). #mine\n";
    const ARG: &str = "---\nid: arg\nkind: argument\ntitle: \"A\"\nthesis: \"t\"\nrelations:\n  - { to: sla#e1, stance: supports, op: analyze }\n---\n";
    const SRC: &str = "---\nid: wright2004\nkind: source\n---\n";
    const FAR: &str = "---\nid: far\nkind: argument\nthesis: \"t\"\nrelations:\n  - { to: lmr, stance: opposes }\n---\n";

    fn check(id: &str, hops: u32, expected: Expect) {
        let (snap, _) = snapshot(
            &[
                ("/v/sla.md", SLA.replacen("id: sla", "id: s$0la", 1).leak()),
                ("/v/lmr.md", LMR),
                ("/v/arg.md", ARG),
                ("/v/src.md", SRC),
                ("/v/far.md", FAR),
            ],
            &["wright2004"],
        );
        let rendered = match handle(
            &snap,
            NeighbourhoodParams {
                id: Some(id.to_owned()),
                uri: None,
                hops,
            },
        ) {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap(),
            Err((_, message)) => format!("error: {message}"),
        };
        expected.assert_eq(&rendered);
    }

    #[test]
    fn one_hop_subgraph_matches_the_dot_shape() {
        check(
            "sla",
            1,
            expect![[r#"
                {
                  "edges": [
                    {
                      "dst": "sla",
                      "kind": "argues",
                      "op": "analyze",
                      "src": "arg",
                      "stance": "supports"
                    },
                    {
                      "dst": "sla",
                      "kind": "mentions",
                      "op": null,
                      "src": "lmr",
                      "stance": null
                    },
                    {
                      "dst": "lmr",
                      "kind": "related",
                      "op": null,
                      "src": "sla",
                      "stance": null
                    },
                    {
                      "dst": "wright2004",
                      "kind": "cites",
                      "op": null,
                      "src": "sla",
                      "stance": null
                    }
                  ],
                  "hops": 1,
                  "nodes": [
                    {
                      "id": "arg",
                      "kind": "argument",
                      "title": "A"
                    },
                    {
                      "id": "lmr",
                      "kind": "topic",
                      "title": "LMR"
                    },
                    {
                      "id": "sla",
                      "kind": "topic",
                      "title": "SLA"
                    },
                    {
                      "id": "wright2004",
                      "kind": "source",
                      "title": null
                    }
                  ],
                  "root": "sla"
                }"#]],
        );
    }

    #[test]
    fn hops_bound_the_subgraph() {
        // 2 hops from the source note reach the whole connected component.
        check(
            "wright2004",
            2,
            expect![[r#"
                {
                  "edges": [
                    {
                      "dst": "sla",
                      "kind": "argues",
                      "op": "analyze",
                      "src": "arg",
                      "stance": "supports"
                    },
                    {
                      "dst": "sla",
                      "kind": "mentions",
                      "op": null,
                      "src": "lmr",
                      "stance": null
                    },
                    {
                      "dst": "lmr",
                      "kind": "related",
                      "op": null,
                      "src": "sla",
                      "stance": null
                    },
                    {
                      "dst": "wright2004",
                      "kind": "cites",
                      "op": null,
                      "src": "sla",
                      "stance": null
                    }
                  ],
                  "hops": 2,
                  "nodes": [
                    {
                      "id": "arg",
                      "kind": "argument",
                      "title": "A"
                    },
                    {
                      "id": "lmr",
                      "kind": "topic",
                      "title": "LMR"
                    },
                    {
                      "id": "sla",
                      "kind": "topic",
                      "title": "SLA"
                    },
                    {
                      "id": "wright2004",
                      "kind": "source",
                      "title": null
                    }
                  ],
                  "root": "wright2004"
                }"#]],
        );
    }

    #[test]
    fn unknown_root_errors() {
        check(
            "nope",
            1,
            expect!["error: `nope` does not resolve to any note"],
        );
    }
}

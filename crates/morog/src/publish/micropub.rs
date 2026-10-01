//! `morog publish micropub` — one JSON-LD micropublication bundle per
//! published argument (plan §1.3, §6.2).
//!
//! The mapping follows §1.3: the **argument is the micropublication** and
//! its `thesis` the principal claim (`mp:Claim`); the argument's typed
//! relations to published topic blocks form the **support graph**, with
//! evidence blocks as `mp:Statement`s — `supports` → `mp:supports`,
//! `opposes` → `mp:challenges`, and `qualifies` (which has no
//! micropublication counterpart) noted via `morog:stance`; the blocks'
//! citekeys carry the **attributions**: each distinct citekey becomes a
//! bibliographic reference node with the metadata `bib_entries` holds, and
//! statements point at their references through `prov:wasDerivedFrom`.
//!
//! Private targets never enter a bundle. Nanopublications stay deferred
//! (plan §6.2).

use std::collections::BTreeMap;
use std::path::PathBuf;

use morog_sema::{NoteKind, Stance};
use serde_json::json;

use super::{Block, Note, PublicGraph, Relation, strip_tags};

/// The micropublications OWL vocabulary [clark2014micropub].
const MP_NS: &str = "http://purl.org/mp/";

pub(super) fn run(vault: Option<PathBuf>, out_dir: PathBuf, format: &str) -> Result<(), String> {
    let (data, _config) = super::load(vault)?;
    let graph = PublicGraph::new(&data)?;

    let arguments: Vec<&Note> = graph
        .published
        .iter()
        .copied()
        .filter(|n| n.kind == NoteKind::Argument)
        .collect();

    std::fs::create_dir_all(&out_dir)
        .map_err(|e| format!("cannot create {}: {e}", out_dir.display()))?;
    for argument in &arguments {
        let bundle = bundle(&graph, argument)?;
        let text = serde_json::to_string_pretty(&bundle)
            .map_err(|e| format!("cannot serialize bundle for `{}`: {e}", argument.id))?;
        let path = out_dir.join(format!("{}.jsonld", argument.id));
        std::fs::write(&path, text + "\n")
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }

    if format == "json" {
        println!(
            "{{\"bundles\": {}, \"dir\": \"{}\"}}",
            arguments.len(),
            crate::json::escape(&out_dir.display().to_string()),
        );
    } else {
        println!(
            "wrote {} micropublication bundle(s) to {}",
            arguments.len(),
            out_dir.display()
        );
    }
    Ok(())
}

/// Build one argument's bundle: micropublication + claim + support graph +
/// references, as a `@graph` document with deterministic node order.
fn bundle(graph: &PublicGraph<'_>, argument: &Note) -> Result<serde_json::Value, String> {
    let iri = graph.iri(argument)?;
    let claim_id = format!("{iri}#claim");

    // Support-graph statements keyed by node id, references keyed by
    // citekey — BTreeMaps, so the emitted graph is ordered.
    let mut statements: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
        BTreeMap::new();
    let mut references: BTreeMap<String, String> = BTreeMap::new(); // citekey → node id

    for rel in graph
        .data
        .relations
        .iter()
        .filter(|r| r.src == argument.id && r.kind == "argues")
    {
        let Some(topic) = graph.note(&rel.dst).filter(|n| n.publish) else {
            continue; // private or unresolved target (§6.1)
        };
        let topic_iri = graph.iri(topic)?;
        let blocks = target_blocks(topic, rel.dst_anchor.as_deref());
        if blocks.is_empty() {
            // No addressable evidence: the topic itself is the statement.
            let entry = statements
                .entry(topic_iri.clone())
                .or_insert_with(|| statement_node(&topic_iri, topic.display_title()));
            add_stance(entry, rel, &claim_id);
            continue;
        }
        for block in blocks {
            let anchor = block.anchor.as_deref().expect("anchored evidence");
            let block_id = format!("{topic_iri}#{anchor}");
            let entry = statements.entry(block_id.clone()).or_insert_with(|| {
                let text = strip_tags(&block.text, block.anchor.as_deref());
                statement_node(&block_id, text.trim())
            });
            add_stance(entry, rel, &claim_id);
            // Attributions: the block's citekeys → reference nodes.
            let mut derived: Vec<String> = Vec::new();
            for cite in graph.block_cites(topic, block) {
                let ref_id = reference_id(graph, &iri, &cite.dst);
                derived.push(ref_id.clone());
                references.insert(cite.dst.clone(), ref_id);
            }
            derived.sort();
            derived.dedup();
            if !derived.is_empty() {
                entry.insert(
                    "prov:wasDerivedFrom".into(),
                    json!(
                        derived
                            .iter()
                            .map(|id| json!({ "@id": id }))
                            .collect::<Vec<_>>()
                    ),
                );
            }
        }
    }

    let mut nodes: Vec<serde_json::Value> = Vec::new();
    let mut root = serde_json::Map::new();
    root.insert("@id".into(), json!(iri));
    root.insert("@type".into(), json!("mp:Micropublication"));
    root.insert("name".into(), json!(argument.display_title()));
    if let Some(created) = &argument.created {
        root.insert("dcterms:created".into(), json!(created));
    }
    root.insert("mp:argues".into(), json!({ "@id": claim_id }));
    nodes.push(serde_json::Value::Object(root));

    let mut claim = serde_json::Map::new();
    claim.insert("@id".into(), json!(claim_id));
    claim.insert("@type".into(), json!("mp:Claim"));
    claim.insert(
        "text".into(),
        json!(
            argument
                .thesis
                .as_deref()
                .unwrap_or(argument.display_title())
        ),
    );
    if let Some(form) = &argument.form {
        claim.insert("morog:form".into(), json!(form));
    }
    nodes.push(serde_json::Value::Object(claim));

    nodes.extend(statements.into_values().map(serde_json::Value::Object));
    for (key, ref_id) in &references {
        nodes.push(reference_node(graph, key, ref_id));
    }

    Ok(json!({
        "@context": {
            "mp": MP_NS,
            "morog": super::MOROG_NS,
            "cito": super::CITO_NS,
            "prov": "http://www.w3.org/ns/prov#",
            "dcterms": "http://purl.org/dc/terms/",
            "name": "https://schema.org/name",
            "text": "https://schema.org/text",
        },
        "@graph": nodes,
    }))
}

/// The blocks a relation addresses: the anchored block itself, or every
/// anchored evidence block of the topic for a whole-topic target.
fn target_blocks<'a>(topic: &'a Note, anchor: Option<&str>) -> Vec<&'a Block> {
    match anchor {
        Some(anchor) => topic
            .blocks
            .iter()
            .filter(|b| b.anchor.as_deref() == Some(anchor))
            .collect(),
        None => topic
            .blocks
            .iter()
            .filter(|b| b.unit == "evidence" && b.anchor.is_some())
            .collect(),
    }
}

fn statement_node(id: &str, text: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut node = serde_json::Map::new();
    node.insert("@id".into(), json!(id));
    node.insert("@type".into(), json!("mp:Statement"));
    node.insert("text".into(), json!(text));
    node
}

/// Record one relation's stance on a statement node: `mp:supports` /
/// `mp:challenges` edges to the claim, `qualifies` via `morog:stance`
/// (plan §1.3 — micropublications have no qualifier edge), and the Bloom
/// `morog:op` when declared.
fn add_stance(
    node: &mut serde_json::Map<String, serde_json::Value>,
    rel: &Relation,
    claim_id: &str,
) {
    let predicate = match rel.stance {
        Some(Stance::Supports) => "mp:supports",
        Some(Stance::Opposes) => "mp:challenges",
        Some(Stance::Qualifies) | None => {
            node.insert("morog:stance".into(), json!("qualifies"));
            node.insert("morog:qualifies".into(), json!({ "@id": claim_id }));
            if let Some(op) = rel.op {
                node.insert("morog:op".into(), json!(op.as_str()));
            }
            return;
        }
    };
    node.insert(predicate.into(), json!({ "@id": claim_id }));
    if let Some(op) = rel.op {
        node.insert("morog:op".into(), json!(op.as_str()));
    }
}

/// The node id of a citekey's reference: the published source note's IRI,
/// else the DOI, else a bundle-local fragment.
fn reference_id(graph: &PublicGraph<'_>, bundle_iri: &str, citekey: &str) -> String {
    if let Some(iri) = graph.iri_for(citekey, None) {
        return iri;
    }
    if let Some(doi) = graph.data.bib.get(citekey).and_then(|b| b.doi.as_deref()) {
        return format!("https://doi.org/{doi}");
    }
    format!("{bundle_iri}#ref-{citekey}")
}

/// A bibliographic reference node carrying the attribution metadata the
/// bibliography holds for this citekey (plan §1.3).
fn reference_node(graph: &PublicGraph<'_>, citekey: &str, ref_id: &str) -> serde_json::Value {
    let mut node = serde_json::Map::new();
    node.insert("@id".into(), json!(ref_id));
    node.insert("@type".into(), json!("dcterms:BibliographicResource"));
    node.insert("morog:citekey".into(), json!(citekey));
    if let Some(meta) = graph.data.bib.get(citekey) {
        if let Some(title) = &meta.title {
            node.insert("name".into(), json!(title));
        }
        if let Some(year) = meta.year {
            node.insert("dcterms:date".into(), json!(year.to_string()));
        }
        if let Some(doi) = &meta.doi {
            node.insert(
                "dcterms:identifier".into(),
                json!(format!("https://doi.org/{doi}")),
            );
        }
    }
    serde_json::Value::Object(node)
}

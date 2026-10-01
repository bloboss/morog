//! `morog publish rdf` — a Turtle dump of the published subgraph
//! (plan §6.2), for triple stores and the ORKG.
//!
//! Vocabulary (plan §6.1): notes are resources under `base_iri` typed
//! `morog:Source` / `morog:Topic` / `morog:Argument`; typed relations are
//! blank nodes carrying `morog:stance` and `morog:op` (as vocabulary IRIs);
//! anchored evidence blocks are `morog:EvidenceBlock` fragment resources
//! with PROV-O derivations back to their sources; citations with a DOI in
//! `bib_entries` are CiTO `cito:cites` triples.
//!
//! The emitter is hand-rolled, matching the repository's DOT and JSON
//! emitters: the correctness-critical pieces — literal and IRI escaping,
//! stable ordering — are small, local, and unit-tested, and the integration
//! test parses the output with a real Turtle parser (`oxttl`). Statements
//! are emitted in (note id, span) order, so a regenerated dump diffs
//! cleanly.

use std::fmt::Write as _;
use std::path::PathBuf;

use morog_sema::NoteKind;

use super::{Note, PublicGraph, strip_tags};

const PREFIXES: &str = "\
@prefix cito: <http://purl.org/spar/cito/> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix morog: <https://morog.example/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
";

pub(super) fn run(vault: Option<PathBuf>, output: Option<PathBuf>) -> Result<(), String> {
    let (data, _config) = super::load(vault)?;
    let graph = PublicGraph::new(&data)?;
    let text = emit(&graph)?;
    match output {
        Some(path) => {
            std::fs::write(&path, &text)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
            eprintln!(
                "wrote {} published note(s) as Turtle to {}",
                graph.published.len(),
                path.display()
            );
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn emit(graph: &PublicGraph<'_>) -> Result<String, String> {
    let mut out = String::from(PREFIXES);

    for note in &graph.published {
        let iri = graph.iri(note)?;
        out.push('\n');
        note_statement(graph, note, &iri, &mut out);
        relation_statements(graph, note, &iri, &mut out);
        if note.kind == NoteKind::Topic {
            evidence_statements(graph, note, &iri, &mut out);
        }
    }
    Ok(out)
}

/// The note resource itself: type, label, title, and scalar metadata.
fn note_statement(graph: &PublicGraph<'_>, note: &Note, iri: &str, out: &mut String) {
    let class = match note.kind {
        NoteKind::Source => "morog:Source",
        NoteKind::Topic => "morog:Topic",
        NoteKind::Argument => "morog:Argument",
    };
    let _ = write!(out, "{} a {class}", iri_ref(iri));
    let _ = write!(out, " ;\n    rdfs:label {}", literal(&note.id));
    if let Some(title) = &note.title {
        let _ = write!(out, " ;\n    dcterms:title {}", literal(title));
    }
    if let Some(created) = &note.created {
        let _ = write!(out, " ;\n    dcterms:created {}", literal(created));
    }
    for alias in &note.aliases {
        let _ = write!(out, " ;\n    morog:alias {}", literal(alias));
    }
    if let Some(status) = &note.status {
        let _ = write!(out, " ;\n    morog:status {}", literal(status));
    }
    if let Some(form) = &note.form {
        let _ = write!(out, " ;\n    morog:form {}", literal(form));
    }
    if let Some(thesis) = &note.thesis {
        let _ = write!(out, " ;\n    morog:thesis {}", literal(thesis));
    }
    // The readable id doubles as the human URL slug (plan §6.1: label +
    // redirect); record the alias page as a plain-web identifier.
    let _ = write!(
        out,
        " ;\n    rdfs:seeAlso {}",
        iri_ref(&format!("{}/{}", graph.data.base_iri, note.id))
    );
    out.push_str(" .\n");
}

/// Declared and inline relations whose target is published: `argues` as a
/// blank node with stance/op, `related` and `mentions` as direct triples.
fn relation_statements(graph: &PublicGraph<'_>, note: &Note, iri: &str, out: &mut String) {
    for rel in graph
        .data
        .relations
        .iter()
        .filter(|r| r.src == note.id && r.kind != "cites")
    {
        let Some(target) = graph.iri_for(&rel.dst, rel.dst_anchor.as_deref()) else {
            continue; // private or unresolved target: stays out (§6.1)
        };
        match rel.kind.as_str() {
            "argues" => {
                let _ = write!(
                    out,
                    "{} morog:argues [\n    morog:target {}",
                    iri_ref(iri),
                    iri_ref(&target)
                );
                if let Some(stance) = rel.stance {
                    let _ = write!(out, " ;\n    morog:stance morog:{}", stance.as_str());
                }
                if let Some(op) = rel.op {
                    let _ = write!(out, " ;\n    morog:op morog:{}", op.as_str());
                }
                out.push_str("\n] .\n");
            }
            "related" => {
                let _ = writeln!(out, "{} morog:related {} .", iri_ref(iri), iri_ref(&target));
            }
            _ => {
                let _ = writeln!(
                    out,
                    "{} morog:mentions {} .",
                    iri_ref(iri),
                    iri_ref(&target)
                );
            }
        }
    }
}

/// Evidence blocks of a published topic. Anchored blocks become fragment
/// resources with their text, a PROV-O derivation per resolvable citation,
/// and CiTO triples where the bibliography carries a DOI; citations in
/// anchorless blocks attach their CiTO triple to the topic itself.
fn evidence_statements(graph: &PublicGraph<'_>, note: &Note, iri: &str, out: &mut String) {
    for block in &note.blocks {
        if block.unit != "evidence" {
            continue;
        }
        let cites = graph.block_cites(note, block);
        let subject = match &block.anchor {
            Some(anchor) => format!("{iri}#{anchor}"),
            None => {
                // No stable address (plan §2.4): cite from the topic.
                for rel in &cites {
                    if let Some(doi_iri) = doi_iri(graph, &rel.dst) {
                        let _ = writeln!(out, "{} cito:cites {} .", iri_ref(iri), doi_iri);
                    }
                }
                continue;
            }
        };

        let text = strip_tags(&block.text, block.anchor.as_deref());
        let _ = write!(
            out,
            "{} a morog:EvidenceBlock ;\n    dcterms:isPartOf {} ;\n    morog:text {}",
            iri_ref(&subject),
            iri_ref(iri),
            literal(text.trim())
        );
        // Derivation (PROV-O, §6.1): the published source note when one
        // exists, else the DOI. Bib-only keys without a DOI have no IRI to
        // point at and are skipped.
        for rel in &cites {
            let derived = graph
                .iri_for(&rel.dst, None)
                .map(|iri| iri_ref(&iri))
                .or_else(|| doi_iri(graph, &rel.dst));
            if let Some(derived) = derived {
                let _ = write!(out, " ;\n    prov:wasDerivedFrom {derived}");
            }
        }
        for rel in &cites {
            if let Some(doi_iri) = doi_iri(graph, &rel.dst) {
                let _ = write!(out, " ;\n    cito:cites {doi_iri}");
            }
        }
        out.push_str(" .\n");
    }
}

/// `<https://doi.org/…>` for a citekey whose bibliography entry has a DOI.
fn doi_iri(graph: &PublicGraph<'_>, citekey: &str) -> Option<String> {
    let doi = graph.data.bib.get(citekey)?.doi.as_deref()?;
    Some(iri_ref(&format!("https://doi.org/{doi}")))
}

/// A Turtle IRI reference: angle brackets with the characters that cannot
/// appear inside one percent-encoded (RFC 3987 / Turtle grammar).
fn iri_ref(iri: &str) -> String {
    let mut out = String::with_capacity(iri.len() + 2);
    out.push('<');
    for c in iri.chars() {
        match c {
            '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\\' => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    let _ = write!(out, "%{byte:02X}");
                }
            }
            c if (c as u32) <= 0x20 => {
                let mut buf = [0u8; 4];
                for byte in c.encode_utf8(&mut buf).bytes() {
                    let _ = write!(out, "%{byte:02X}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('>');
    out
}

/// A Turtle string literal (double-quoted, single-line escapes).
fn literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_escaping() {
        assert_eq!(literal("plain"), "\"plain\"");
        assert_eq!(literal("a \"quote\""), "\"a \\\"quote\\\"\"");
        assert_eq!(literal("back\\slash"), "\"back\\\\slash\"");
        assert_eq!(literal("two\nlines\tand\r"), "\"two\\nlines\\tand\\r\"");
        assert_eq!(literal("café 🦀"), "\"café 🦀\"");
    }

    #[test]
    fn iri_escaping() {
        assert_eq!(
            iri_ref("https://x.example/n/a-b"),
            "<https://x.example/n/a-b>"
        );
        assert_eq!(
            iri_ref("https://doi.org/10.1000/a<b> c"),
            "<https://doi.org/10.1000/a%3Cb%3E%20c>"
        );
        assert_eq!(
            iri_ref("https://x.example/{}|^`"),
            "<https://x.example/%7B%7D%7C%5E%60>"
        );
    }
}

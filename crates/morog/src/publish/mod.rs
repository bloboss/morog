//! `morog publish` — the M8 exporters (plan §6): a static site, an RDF
//! dump, and micropublication bundles.
//!
//! Every target reads the **index views of an existing index** and never
//! reparses markdown (plan §6.2): note identity comes from `notes`, body
//! text from `blocks`, edges from `relations`, and bibliography metadata
//! from `bib_entries`. Only notes with `publish: true` are exported; their
//! public IRIs are minted from the `uuid` beneath the configured
//! `[publish] base_iri` (plan §6.1), which is why publishing demands both.
//!
//! Output is deterministic — every iteration is over sorted data — so a
//! regenerated site or dump diffs cleanly.

mod rdf;
mod site;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use morog_index::{QueryOutput, Value};
use morog_sema::{BloomOp, NoteKind, Stance};

use crate::cli::PublishCommand;

/// morog's small vocabulary (plan §6.1: `morog:stance`, `morog:op`, …).
/// `.example` is reserved by RFC 2606; the namespace moves to a real domain
/// once the project owns one.
pub(crate) const MOROG_NS: &str = "https://morog.example/ns#";
/// CiTO, for citations with a DOI (plan §6.1).
pub(crate) const CITO_NS: &str = "http://purl.org/spar/cito/";

pub fn run(command: PublishCommand, format: &str) -> ExitCode {
    let result = match command {
        PublishCommand::Site { output, vault } => site::run(
            vault,
            output.unwrap_or_else(|| PathBuf::from("_site")),
            format,
        ),
        PublishCommand::Rdf { output, vault } => rdf::run(vault, output),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("morog publish: {message}");
            ExitCode::FAILURE
        }
    }
}

// ------------------------------------------------------------------- model

/// Everything the exporters read, loaded once from the index.
pub(crate) struct PublishData {
    /// `[publish] base_iri`, trailing slash trimmed.
    pub base_iri: String,
    /// Every note in the vault — private ones included, so exporters can
    /// tell a private target apart from a bib virtual source. Sorted by id.
    pub notes: Vec<Note>,
    /// Every relation with a known source note (frontmatter `argues` /
    /// `related` plus inline `cites` / `mentions`), ordered by source and
    /// span.
    pub relations: Vec<Relation>,
    /// Bibliography metadata by citekey.
    pub bib: BTreeMap<String, BibMeta>,
}

pub(crate) struct Note {
    pub id: String,
    pub uuid: Option<String>,
    pub kind: NoteKind,
    pub title: Option<String>,
    pub form: Option<String>,
    pub thesis: Option<String>,
    pub status: Option<String>,
    pub created: Option<String>,
    /// The `extra:` map as JSON (schema v2), exposed to templates.
    pub extra_json: Option<String>,
    pub publish: bool,
    pub aliases: Vec<String>,
    /// Top-level blocks in document order.
    pub blocks: Vec<Block>,
}

impl Note {
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.id)
    }
}

pub(crate) struct Block {
    pub idx: i64,
    pub anchor: Option<String>,
    pub unit: String,
    pub citable: bool,
    /// The raw source slice of the block (plan §6.2: block text comes from
    /// the `blocks` table).
    pub text: String,
    /// Byte offsets into the original file, for inline-segment surgery.
    pub start: i64,
    pub end: i64,
}

pub(crate) struct Relation {
    pub src: String,
    pub src_block: Option<i64>,
    pub dst: String,
    pub dst_anchor: Option<String>,
    /// `argues`, `related`, `cites`, or `mentions`.
    pub kind: String,
    pub stance: Option<Stance>,
    pub op: Option<BloomOp>,
    pub locator: Option<String>,
    pub start: i64,
    pub end: i64,
}

pub(crate) struct BibMeta {
    pub title: Option<String>,
    pub year: Option<i64>,
    pub doi: Option<String>,
}

/// The public IRI of a published note (plan §6.1).
pub(crate) fn note_iri(base: &str, uuid: &str) -> String {
    format!("{base}/n/{uuid}")
}

/// Strip the inline tags the exporters consume from a literal text chunk:
/// the block's own `#anchor <name>` tag and `#mine`.
pub(crate) fn strip_tags(chunk: &str, anchor: Option<&str>) -> String {
    let mut text = chunk.to_owned();
    if let Some(anchor) = anchor {
        text = text.replace(&format!("#anchor {anchor}"), "");
    }
    text.replace("#mine", "")
}

/// The published subgraph, as every exporter sees it: the `publish: true`
/// notes (validated to carry the uuids their IRIs are minted from, §6.1),
/// id lookup over the whole vault, and IRI resolution that refuses private
/// targets.
pub(crate) struct PublicGraph<'a> {
    pub data: &'a PublishData,
    /// Published notes, sorted by id.
    pub published: Vec<&'a Note>,
    /// id → note over the whole vault (private notes included).
    by_id: BTreeMap<&'a str, &'a Note>,
}

impl<'a> PublicGraph<'a> {
    pub fn new(data: &'a PublishData) -> Result<Self, String> {
        let published: Vec<&Note> = data.notes.iter().filter(|n| n.publish).collect();
        let missing: Vec<&str> = published
            .iter()
            .filter(|n| n.uuid.is_none())
            .map(|n| n.id.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "published note(s) without a uuid: {}; public IRIs are minted \
                 from the uuid (plan §6.1), add a `uuid:` field",
                missing.join(", ")
            ));
        }
        Ok(Self {
            data,
            published,
            by_id: data.notes.iter().map(|n| (n.id.as_str(), n)).collect(),
        })
    }

    /// Look any vault note up by id.
    pub fn note(&self, id: &str) -> Option<&'a Note> {
        self.by_id.get(id).copied()
    }

    pub fn uuid_of(&self, note: &'a Note) -> Result<&'a str, String> {
        note.uuid
            .as_deref()
            .ok_or_else(|| format!("note `{}` has no uuid", note.id))
    }

    /// The canonical IRI of a published note.
    pub fn iri(&self, note: &'a Note) -> Result<String, String> {
        Ok(note_iri(&self.data.base_iri, self.uuid_of(note)?))
    }

    /// The IRI of a target id (+ optional anchor) — `None` unless the
    /// target is a published note (private targets stay out of the public
    /// graph, §6.1).
    pub fn iri_for(&self, target: &str, anchor: Option<&str>) -> Option<String> {
        let note = self.note(target)?;
        if !note.publish {
            return None;
        }
        let iri = note_iri(&self.data.base_iri, note.uuid.as_deref()?);
        Some(match anchor {
            Some(a) => format!("{iri}#{a}"),
            None => iri,
        })
    }

    /// A note's declared (frontmatter) relations: `argues` and `related`.
    pub fn declared_relations(&self, note: &Note) -> Vec<&'a Relation> {
        self.data
            .relations
            .iter()
            .filter(|r| r.src == note.id && matches!(r.kind.as_str(), "argues" | "related"))
            .collect()
    }

    /// Inline citations of one block, in span order.
    pub fn block_cites(&self, note: &Note, block: &Block) -> Vec<&'a Relation> {
        self.data
            .relations
            .iter()
            .filter(|r| r.src == note.id && r.src_block == Some(block.idx) && r.kind == "cites")
            .collect()
    }
}

// ----------------------------------------------------------------- loading

/// Resolve the vault, require `[publish] base_iri`, and load the exporter
/// model from the existing index. Returns the data plus the merged config
/// (the site needs `site_dir`).
pub(crate) fn load(vault: Option<PathBuf>) -> Result<(PublishData, crate::config::Config), String> {
    let vault = vault.unwrap_or_else(|| PathBuf::from("."));
    let vault = vault
        .canonicalize()
        .map_err(|e| format!("cannot open vault {}: {e}", vault.display()))?;
    let config = crate::config::load(&vault)?;
    let Some(base_iri) = config.publish.base_iri.clone() else {
        return Err(format!(
            "no [publish] base_iri configured; set it in {} at the vault root \
             or in ~/.config/morog/config.toml (plan §6.1)",
            crate::config::VAULT_FILE
        ));
    };
    let base_iri = base_iri.trim_end_matches('/').to_owned();

    // Require an existing index, like `morog graph` (plan §3.1: the index is
    // built explicitly, never as a side effect).
    let db_path = morog_index::cache_db_path(&vault);

    let mut notes = load_notes(&db_path)?;
    load_aliases(&db_path, &mut notes)?;
    load_blocks(&db_path, &mut notes)?;
    let relations = load_relations(&db_path)?;
    let bib = load_bib(&db_path)?;

    Ok((
        PublishData {
            base_iri,
            notes,
            relations,
            bib,
        },
        config,
    ))
}

fn query(db_path: &Path, sql: &str) -> Result<QueryOutput, String> {
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

/// Notes sorted by id; `file_rank` maps each note's `file_id` to its index
/// in the returned vector so blocks can be attached without a second join.
fn load_notes(db_path: &Path) -> Result<Vec<Note>, String> {
    let out = query(
        db_path,
        "SELECT note_id, uuid, kind, title, form, thesis, status, created, extra,
                publish, file_id
         FROM notes ORDER BY note_id",
    )?;
    out.rows
        .iter()
        .map(|row| {
            let id = text_cell(&row[0]).ok_or("corrupt index: non-text note_id")?;
            let kind = text_cell(&row[2])
                .as_deref()
                .and_then(NoteKind::parse)
                .ok_or_else(|| format!("corrupt index: bad kind on note `{id}`"))?;
            Ok(Note {
                id,
                uuid: text_cell(&row[1]),
                kind,
                title: text_cell(&row[3]),
                form: text_cell(&row[4]),
                thesis: text_cell(&row[5]),
                status: text_cell(&row[6]),
                created: text_cell(&row[7]),
                extra_json: text_cell(&row[8]),
                publish: int_cell(&row[9]) == Some(1),
                aliases: Vec::new(),
                blocks: Vec::new(),
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|mut notes| {
            notes.sort_by(|a, b| a.id.cmp(&b.id));
            notes
        })
}

fn load_aliases(db_path: &Path, notes: &mut [Note]) -> Result<(), String> {
    let out = query(
        db_path,
        "SELECT note_id, alias FROM aliases WHERE note_id IS NOT NULL ORDER BY note_id, ord",
    )?;
    for row in &out.rows {
        let (Some(id), Some(alias)) = (text_cell(&row[0]), text_cell(&row[1])) else {
            continue;
        };
        if let Ok(i) = notes.binary_search_by(|n| n.id.as_str().cmp(id.as_str())) {
            notes[i].aliases.push(alias);
        }
    }
    Ok(())
}

fn load_blocks(db_path: &Path, notes: &mut [Note]) -> Result<(), String> {
    let out = query(
        db_path,
        "SELECT n.note_id, b.idx, b.anchor, b.unit, b.citable, b.text,
                b.start_byte, b.end_byte
         FROM blocks b JOIN notes n ON n.file_id = b.file_id
         ORDER BY n.note_id, b.idx",
    )?;
    for row in &out.rows {
        let Some(id) = text_cell(&row[0]) else {
            continue;
        };
        let Ok(i) = notes.binary_search_by(|n| n.id.as_str().cmp(id.as_str())) else {
            continue;
        };
        notes[i].blocks.push(Block {
            idx: int_cell(&row[1]).unwrap_or(0),
            anchor: text_cell(&row[2]),
            unit: text_cell(&row[3]).unwrap_or_else(|| "none".into()),
            citable: int_cell(&row[4]) == Some(1),
            text: text_cell(&row[5]).unwrap_or_default(),
            start: int_cell(&row[6]).unwrap_or(0),
            end: int_cell(&row[7]).unwrap_or(0),
        });
    }
    Ok(())
}

fn load_relations(db_path: &Path) -> Result<Vec<Relation>, String> {
    let out = query(
        db_path,
        "SELECT src_note, src_block, dst, dst_anchor, kind, stance, op, locator,
                start_byte, end_byte
         FROM relations WHERE src_note IS NOT NULL
         ORDER BY src_note, start_byte, dst",
    )?;
    out.rows
        .iter()
        .map(|row| {
            Ok(Relation {
                src: text_cell(&row[0]).ok_or("corrupt index: non-text relation src")?,
                src_block: int_cell(&row[1]),
                dst: text_cell(&row[2]).ok_or("corrupt index: non-text relation dst")?,
                dst_anchor: text_cell(&row[3]),
                kind: text_cell(&row[4]).ok_or("corrupt index: non-text relation kind")?,
                stance: text_cell(&row[5]).as_deref().and_then(Stance::parse),
                op: int_cell(&row[6])
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(|n| BloomOp::ALL.get(n).copied())
                    .and_then(BloomOp::parse),
                locator: text_cell(&row[7]),
                start: int_cell(&row[8]).unwrap_or(0),
                end: int_cell(&row[9]).unwrap_or(0),
            })
        })
        .collect()
}

fn load_bib(db_path: &Path) -> Result<BTreeMap<String, BibMeta>, String> {
    let out = query(
        db_path,
        "SELECT citekey, title, year, doi FROM bib_entries ORDER BY citekey",
    )?;
    Ok(out
        .rows
        .iter()
        .filter_map(|row| {
            let key = text_cell(&row[0])?;
            Some((
                key,
                BibMeta {
                    title: text_cell(&row[1]),
                    year: int_cell(&row[2]),
                    doi: text_cell(&row[3]),
                },
            ))
        })
        .collect())
}

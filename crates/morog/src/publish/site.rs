//! `morog publish site` — the static site (plan §6.2–6.3).
//!
//! One page per published note at `/n/<uuid>/` (the uuid-minted IRI is
//! canonical, plan §6.1) with the readable id as a redirect alias page at
//! `/<id>/`, an index page listing published notes by kind, backlinks, and
//! JSON-LD embedded in every note page.
//!
//! Rendering is MiniJinja over named template snippets (plan §6.3,
//! eleventy-style): the embedded default theme supplies `base.html`,
//! `note.html`, `backlinks.html`, `index.html`, `alias.html`, and
//! `style.css`; a file with the same name in `[publish] site_dir` replaces
//! that snippet. Note frontmatter — including the `extra:` map — is exposed
//! to templates.
//!
//! Body HTML is derived from the `blocks` table without reparsing markdown
//! (plan §6.2): the stored block text is escaped, and the inline `cites` /
//! `mentions` relation spans are spliced back in as links. A link whose
//! target is not published is rendered as unlinked text (the `pub::leak`
//! lint warns about those).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use minijinja::Environment;
use morog_sema::NoteKind;
use serde_json::json;

use super::{BibMeta, Block, Note, PublishData, Relation, note_iri};

/// The default theme snippets; any of these names can be overridden from
/// `[publish] site_dir`.
const DEFAULT_TEMPLATES: &[(&str, &str)] = &[
    ("base.html", include_str!("templates/base.html")),
    ("note.html", include_str!("templates/note.html")),
    ("backlinks.html", include_str!("templates/backlinks.html")),
    ("index.html", include_str!("templates/index.html")),
    ("alias.html", include_str!("templates/alias.html")),
    ("style.css", include_str!("templates/style.css")),
];

const SITE_TITLE: &str = "Research notes";

pub(super) fn run(vault: Option<PathBuf>, out_dir: PathBuf, format: &str) -> Result<(), String> {
    let (data, config) = super::load(vault)?;
    let env = environment(config.publish.site_dir.as_deref())?;
    let site = Site::new(&data)?;

    let mut pages: Vec<(PathBuf, String)> = Vec::new();
    pages.push((PathBuf::from("style.css"), site.render_style(&env)?));
    pages.push((PathBuf::from("index.html"), site.render_index(&env)?));
    for note in &site.published {
        let uuid = site.uuid_of(note)?;
        pages.push((
            Path::new("n").join(uuid).join("index.html"),
            site.render_note(&env, note)?,
        ));
        pages.push((
            Path::new(&note.id).join("index.html"),
            site.render_alias(&env, note)?,
        ));
    }

    for (rel, text) in &pages {
        let path = out_dir.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }

    if format == "json" {
        println!(
            "{{\"published\": {}, \"files\": {}, \"dir\": \"{}\"}}",
            site.published.len(),
            pages.len(),
            crate::json::escape(&out_dir.display().to_string()),
        );
    } else {
        println!(
            "published {} note(s), {} file(s), to {}",
            site.published.len(),
            pages.len(),
            out_dir.display()
        );
    }
    Ok(())
}

/// Build the template environment: the embedded defaults, each replaced by
/// a same-named file from `site_dir` when one exists (plan §6.3).
fn environment(site_dir: Option<&Path>) -> Result<Environment<'static>, String> {
    if let Some(dir) = site_dir
        && !dir.is_dir()
    {
        return Err(format!(
            "[publish] site_dir {} is not a directory",
            dir.display()
        ));
    }
    let mut env = Environment::new();
    for (name, default) in DEFAULT_TEMPLATES {
        let text = match site_dir.map(|dir| dir.join(name)) {
            Some(path) if path.is_file() => std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read template {}: {e}", path.display()))?,
            _ => (*default).to_owned(),
        };
        env.add_template_owned((*name).to_owned(), text)
            .map_err(|e| format!("invalid template {name}: {e}"))?;
    }
    Ok(env)
}

/// The site view over the loaded data: published notes plus lookup maps.
struct Site<'a> {
    data: &'a PublishData,
    published: Vec<&'a Note>,
    /// id → note, every note in the vault (for private-target decisions).
    by_id: BTreeMap<&'a str, &'a Note>,
}

impl<'a> Site<'a> {
    fn new(data: &'a PublishData) -> Result<Self, String> {
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

    fn uuid_of(&self, note: &'a Note) -> Result<&'a str, String> {
        note.uuid
            .as_deref()
            .ok_or_else(|| format!("note `{}` has no uuid", note.id))
    }

    /// The page href of a target id (+ optional anchor) — `None` unless the
    /// target is a published note (unlinked rendering, plan §6.1).
    fn href_for(&self, target: &str, anchor: Option<&str>) -> Option<String> {
        let note = self.by_id.get(target)?;
        if !note.publish {
            return None;
        }
        let iri = note_iri(&self.data.base_iri, note.uuid.as_deref()?);
        Some(match anchor {
            Some(a) => format!("{iri}#{a}"),
            None => iri,
        })
    }

    fn site_context(&self) -> serde_json::Value {
        json!({ "base": self.data.base_iri, "title": SITE_TITLE })
    }

    fn render_style(&self, env: &Environment<'_>) -> Result<String, String> {
        render(env, "style.css", json!({ "site": self.site_context() }))
    }

    fn render_index(&self, env: &Environment<'_>) -> Result<String, String> {
        let group = |kind: NoteKind, heading: &str| {
            json!({
                "heading": heading,
                "notes": self
                    .published
                    .iter()
                    .filter(|n| n.kind == kind)
                    .map(|n| {
                        json!({
                            "id": n.id,
                            "title": n.display_title(),
                            "iri": note_iri(&self.data.base_iri, n.uuid.as_deref().unwrap_or("")),
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        };
        // Index order follows the research direction: arguments first.
        let groups = vec![
            group(NoteKind::Argument, "Arguments"),
            group(NoteKind::Topic, "Topics"),
            group(NoteKind::Source, "Sources"),
        ];
        render(
            env,
            "index.html",
            json!({ "site": self.site_context(), "groups": groups }),
        )
    }

    fn render_alias(&self, env: &Environment<'_>, note: &Note) -> Result<String, String> {
        let iri = note_iri(&self.data.base_iri, self.uuid_of(note)?);
        render(
            env,
            "alias.html",
            json!({ "iri": iri, "title": note.display_title() }),
        )
    }

    fn render_note(&self, env: &Environment<'_>, note: &'a Note) -> Result<String, String> {
        let iri = note_iri(&self.data.base_iri, self.uuid_of(note)?);
        let extra: serde_json::Value = note
            .extra_json
            .as_deref()
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or(serde_json::Value::Null);

        let blocks: Vec<serde_json::Value> = note
            .blocks
            .iter()
            .map(|block| {
                json!({
                    "anchor": block.anchor,
                    "unit": block.unit,
                    "html": self.block_html(note, block),
                })
            })
            .collect();

        let relations: Vec<serde_json::Value> = self
            .declared_relations(note)
            .iter()
            .map(|rel| {
                let label = self
                    .by_id
                    .get(rel.dst.as_str())
                    .map(|n| n.display_title().to_owned())
                    .unwrap_or_else(|| rel.dst.clone());
                json!({
                    "rel": rel.stance.map(|s| s.as_str()).unwrap_or(&rel.kind),
                    "op": rel.op.map(|o| o.as_str()),
                    "label": label,
                    "href": self.href_for(&rel.dst, rel.dst_anchor.as_deref()),
                })
            })
            .collect();

        let backlinks: Vec<serde_json::Value> = self
            .backlinks(note)
            .iter()
            .map(|rel| {
                let src = self.by_id[rel.src.as_str()];
                json!({
                    "label": src.display_title(),
                    "href": self.href_for(&rel.src, None),
                    "rel": rel.stance.map(|s| s.as_str()).unwrap_or(&rel.kind),
                    "op": rel.op.map(|o| o.as_str()),
                })
            })
            .collect();

        let jsonld = self.note_jsonld(note, &iri)?;
        render(
            env,
            "note.html",
            json!({
                "site": self.site_context(),
                "note": {
                    "id": note.id,
                    "uuid": note.uuid,
                    "iri": iri,
                    "kind": note.kind.as_str(),
                    "title": note.display_title(),
                    "form": note.form,
                    "thesis": note.thesis,
                    "status": note.status,
                    "created": note.created,
                    "aliases": note.aliases,
                    "extra": extra,
                },
                "blocks": blocks,
                "relations": relations,
                "backlinks": backlinks,
                "jsonld": jsonld,
            }),
        )
    }

    /// The note's declared (frontmatter) relations: `argues` and `related`.
    fn declared_relations(&self, note: &Note) -> Vec<&'a Relation> {
        self.data
            .relations
            .iter()
            .filter(|r| r.src == note.id && matches!(r.kind.as_str(), "argues" | "related"))
            .collect()
    }

    /// Incoming relations from other published notes, in (src, span) order.
    fn backlinks(&self, note: &Note) -> Vec<&'a Relation> {
        self.data
            .relations
            .iter()
            .filter(|r| {
                r.dst == note.id
                    && r.src != note.id
                    && self.by_id.get(r.src.as_str()).is_some_and(|n| n.publish)
            })
            .collect()
    }

    /// Inline segments (cites and `id:` links) inside one block, by span.
    fn inline_segments(&self, note: &Note, block: &Block) -> Vec<&'a Relation> {
        let mut segs: Vec<&Relation> = self
            .data
            .relations
            .iter()
            .filter(|r| {
                r.src == note.id
                    && r.src_block == Some(block.idx)
                    && matches!(r.kind.as_str(), "cites" | "mentions")
                    && r.start >= block.start
                    && r.end <= block.end
            })
            .collect();
        segs.sort_by_key(|r| r.start);
        segs
    }

    /// One block as HTML: escape the stored text, splice the inline
    /// citation/link spans back in, strip the `#anchor` / `#mine` tags, and
    /// wrap in a tag inferred from the raw text (plan §6.2 — no reparse).
    fn block_html(&self, note: &Note, block: &Block) -> String {
        let text = block.text.as_str();
        let mut inner = String::with_capacity(text.len());
        let mut pos = 0usize;
        for seg in self.inline_segments(note, block) {
            let (start, end) = (
                (seg.start - block.start) as usize,
                (seg.end - block.start) as usize,
            );
            if start < pos || end > text.len() || !text.is_char_boundary(start) {
                continue; // defensive: span outside the stored slice
            }
            inner.push_str(&clean_text(&text[pos..start], block.anchor.as_deref()));
            inner.push_str(&self.segment_html(seg, &text[start..end]));
            pos = end;
        }
        inner.push_str(&clean_text(&text[pos..], block.anchor.as_deref()));
        let inner = inner.trim().to_owned();

        let anchor_attr = block
            .anchor
            .as_deref()
            .map(|a| format!(" id=\"{}\"", escape_attr(a)))
            .unwrap_or_default();

        // Headings and code fences are recognizable from the raw text.
        if !block.citable && text.starts_with('#') {
            let level = text.bytes().take_while(|&b| b == b'#').count();
            let level = (level + 1).clamp(2, 6);
            let content = inner.trim_start_matches('#').trim_start();
            return format!("<h{level}{anchor_attr}>{content}</h{level}>");
        }
        if !block.citable && text.starts_with("```") {
            let code: Vec<&str> = text.lines().collect();
            let body = code[1..code.len().saturating_sub(1)].join("\n");
            return format!(
                "<pre{anchor_attr}><code>{}</code></pre>",
                escape_html(&body)
            );
        }
        let class = match block.unit.as_str() {
            "evidence" => " evidence",
            "mine" => " mine",
            _ => "",
        };
        format!("<p class=\"block{class}\"{anchor_attr}>{inner}</p>")
    }

    /// One inline segment: a `[@key]` citation or an `[text](id:…)` link.
    fn segment_html(&self, seg: &Relation, raw: &str) -> String {
        match seg.kind.as_str() {
            "cites" => {
                // Prefer the published source note's page; else the DOI; else
                // plain text (bib-only keys without a DOI, private sources).
                let href = self.href_for(&seg.dst, None).or_else(|| {
                    self.data
                        .bib
                        .get(&seg.dst)
                        .and_then(|b| b.doi.as_deref())
                        .map(|doi| format!("https://doi.org/{doi}"))
                });
                let key = match href {
                    Some(href) => format!(
                        "<a href=\"{}\">{}</a>",
                        escape_attr(&href),
                        escape_html(&seg.dst)
                    ),
                    None => escape_html(&seg.dst),
                };
                let locator = seg
                    .locator
                    .as_deref()
                    .map(|l| escape_html(&format!(", {l}")))
                    .unwrap_or_default();
                format!("<span class=\"cite\">[{key}{locator}]</span>")
            }
            _ => {
                // `[text](id:target)` — the display text sits between the
                // leading `[` and the `](`.
                let display = raw
                    .strip_prefix('[')
                    .and_then(|r| r.split_once("](").map(|(text, _)| text))
                    .unwrap_or(raw);
                match self.href_for(&seg.dst, seg.dst_anchor.as_deref()) {
                    Some(href) => format!(
                        "<a href=\"{}\">{}</a>",
                        escape_attr(&href),
                        escape_html(display)
                    ),
                    // Private or unresolved target: unlinked text (§6.1).
                    None => escape_html(display),
                }
            }
        }
    }

    /// The JSON-LD island for one note page: schema.org CreativeWork/Claim
    /// plus `morog:stance` / `morog:op` relation terms under a small
    /// context, and CiTO citations where the bibliography has a DOI
    /// (plan §6.1).
    fn note_jsonld(&self, note: &Note, iri: &str) -> Result<String, String> {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "@context".into(),
            json!([
                "https://schema.org",
                { "morog": super::MOROG_NS, "cito": super::CITO_NS }
            ]),
        );
        obj.insert("@id".into(), json!(iri));
        let types = match note.kind {
            NoteKind::Source => json!(["CreativeWork", "morog:Source"]),
            NoteKind::Topic => json!(["CreativeWork", "morog:Topic"]),
            NoteKind::Argument => json!(["Claim", "morog:Argument"]),
        };
        obj.insert("@type".into(), types);
        obj.insert("name".into(), json!(note.display_title()));
        obj.insert("identifier".into(), json!(note.id));
        if !note.aliases.is_empty() {
            obj.insert("alternateName".into(), json!(note.aliases));
        }
        if let Some(created) = &note.created {
            obj.insert("dateCreated".into(), json!(created));
        }
        if let Some(status) = &note.status {
            obj.insert("morog:status".into(), json!(status));
        }
        if let Some(form) = &note.form {
            obj.insert("morog:form".into(), json!(form));
        }
        if let Some(thesis) = &note.thesis {
            obj.insert("text".into(), json!(thesis));
        }

        // Typed relations, published targets only (private ones never leak
        // into the public graph — §6.1).
        let argues: Vec<serde_json::Value> = self
            .declared_relations(note)
            .iter()
            .filter(|r| r.kind == "argues")
            .filter_map(|r| {
                let target = self.href_for(&r.dst, r.dst_anchor.as_deref())?;
                let mut rel = serde_json::Map::new();
                rel.insert("morog:target".into(), json!({ "@id": target }));
                if let Some(stance) = r.stance {
                    rel.insert("morog:stance".into(), json!(stance.as_str()));
                }
                if let Some(op) = r.op {
                    rel.insert("morog:op".into(), json!(op.as_str()));
                }
                Some(serde_json::Value::Object(rel))
            })
            .collect();
        if !argues.is_empty() {
            obj.insert("morog:argues".into(), json!(argues));
        }
        let related: Vec<serde_json::Value> = self
            .declared_relations(note)
            .iter()
            .filter(|r| r.kind == "related")
            .filter_map(|r| Some(json!({ "@id": self.href_for(&r.dst, None)? })))
            .collect();
        if !related.is_empty() {
            obj.insert("morog:related".into(), json!(related));
        }

        // CiTO citations for citekeys whose bibliography entry has a DOI.
        let mut cited: BTreeMap<&str, &BibMeta> = BTreeMap::new();
        for rel in self
            .data
            .relations
            .iter()
            .filter(|r| r.src == note.id && r.kind == "cites")
        {
            if let Some(meta) = self.data.bib.get(&rel.dst)
                && meta.doi.is_some()
            {
                cited.insert(&rel.dst, meta);
            }
        }
        let cites: Vec<serde_json::Value> = cited
            .values()
            .map(|meta| {
                let mut work = serde_json::Map::new();
                let doi = meta.doi.as_deref().expect("filtered on doi");
                work.insert("@id".into(), json!(format!("https://doi.org/{doi}")));
                if let Some(title) = &meta.title {
                    work.insert("name".into(), json!(title));
                }
                if let Some(year) = meta.year {
                    work.insert("datePublished".into(), json!(year.to_string()));
                }
                serde_json::Value::Object(work)
            })
            .collect();
        if !cites.is_empty() {
            obj.insert("cito:cites".into(), json!(cites));
        }

        let text = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
            .map_err(|e| format!("cannot serialize JSON-LD for `{}`: {e}", note.id))?;
        // Keep the inline <script> island intact even if note text contains
        // a closing tag: `<\/` is the same string in JSON.
        Ok(text.replace("</", "<\\/"))
    }
}

fn render(env: &Environment<'_>, name: &str, context: serde_json::Value) -> Result<String, String> {
    let template = env
        .get_template(name)
        .map_err(|e| format!("template {name}: {e}"))?;
    let mut text = template
        .render(context)
        .map_err(|e| format!("template {name}: {e}"))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text)
}

/// Strip the inline tags the exporter consumes (`#anchor <name>`, `#mine`)
/// from a literal chunk, then escape it for HTML.
fn clean_text(chunk: &str, anchor: Option<&str>) -> String {
    let mut text = chunk.to_owned();
    if let Some(anchor) = anchor {
        text = text.replace(&format!("#anchor {anchor}"), "");
    }
    text = text.replace("#mine", "");
    escape_html(&text)
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    escape_html(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_escaping() {
        assert_eq!(escape_html("a & <b>"), "a &amp; &lt;b&gt;");
        assert_eq!(escape_attr("a\"b"), "a&quot;b");
    }

    #[test]
    fn tag_stripping() {
        assert_eq!(
            clean_text("Spectrum. #anchor spectrum", Some("spectrum")),
            "Spectrum. "
        );
        assert_eq!(clean_text("Mine idea. #mine", None), "Mine idea. ");
        // Unrelated tags and inter-word spacing survive.
        assert_eq!(clean_text("Text #other ", Some("a")), "Text #other ");
    }
}

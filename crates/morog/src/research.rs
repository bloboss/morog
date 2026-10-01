//! `morog research` — note creation from the typed STA schemas, a vault
//! listing, and an STA-restricted lint pass (plan §7 M5).
//!
//! Creation serializes frontmatter **from** `morog-sema`'s typed structs
//! ([`NoteMeta`]) and guarantees the result round-trips through
//! `morg-parser` + [`parse_note_meta`] with zero `MetaError`s: the emitted
//! text is re-parsed and verified before the file is written.
//!
//! Duplicate detection scans the vault directly (walk + parse) rather than
//! consulting the SQLite index: the index is disposable and may be stale or
//! absent, while creation is interactive and vaults are small, so an honest
//! re-scan is the simpler correct choice.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use morog_bib::BibStore;
use morog_sema::{
    ArgumentForm, ArgumentMeta, ArgumentStatus, BloomOp, NoteId, NoteKind, NoteMeta, NoteUuid,
    RelationDecl, SourceMeta, SourceStatus, Stance, TopicMeta, is_slug, parse_note_meta,
    split_target,
};

use crate::cli::{BibArgs, ResearchCommand};

pub fn run(command: ResearchCommand, format: &str) -> ExitCode {
    match command {
        ResearchCommand::Source { key, vault, bib } => create_source(&key, vault, &bib, format),
        ResearchCommand::Topic {
            title,
            cites,
            vault,
        } => create_topic(&title, &cites, vault, format),
        ResearchCommand::Argument { title, to, vault } => {
            create_argument(&title, &to, vault, format)
        }
        ResearchCommand::List { vault } => list(vault, format),
        ResearchCommand::Check { files, bib } => {
            crate::lint::run_pass(files, format, &bib, true, "research check")
        }
    }
}

// ---------------------------------------------------------------------------
// Creation

fn create_source(key: &str, vault: Option<PathBuf>, bib_args: &BibArgs, format: &str) -> ExitCode {
    if !is_slug(key) {
        return fail(format_args!("`{key}` is not a slug-shaped citation key"));
    }
    let store = match crate::bib::load(bib_args, "research source") {
        Ok(store) => store,
        Err(message) => return fail(format_args!("{message}")),
    };
    // With a bibliography at hand the key must resolve (plan §4.2: a source
    // note anchors an existing bibrat entry). Without one, creation still
    // works — the `sta::id` lint will police the key once a store appears.
    let title = match &store {
        Some(store) => match store.resolve(&morog_bib::CiteKey::new(key)) {
            Some(entry) => entry.meta.as_ref().and_then(|m| m.title.clone()),
            None => {
                return fail(format_args!(
                    "citation key `{key}` not found in the bibrat store"
                ));
            }
        },
        None => {
            eprintln!(
                "morog research: no bibliography available; creating source `{key}` unchecked"
            );
            None
        }
    };

    let meta = NoteMeta::Source(SourceMeta {
        id: Some(NoteId::new(key)),
        uuid: Some(new_uuid()),
        title,
        status: Some(SourceStatus::Unread),
        created: Some(today_utc()),
        publish: false,
        extra: Vec::new(),
    });
    create_note(key, &meta, "", vault, format)
}

fn create_topic(title: &str, cites: &[String], vault: Option<PathBuf>, format: &str) -> ExitCode {
    let Some(id) = slugify(title) else {
        return fail(format_args!("cannot derive a slug id from `{title}`"));
    };
    for key in cites {
        if !is_slug(key) {
            return fail(format_args!("`{key}` is not a slug-shaped citation key"));
        }
    }

    let meta = NoteMeta::Topic(TopicMeta {
        id: Some(NoteId::new(id.as_str())),
        uuid: Some(new_uuid()),
        title: Some(title.to_owned()),
        aliases: Vec::new(),
        related: Vec::new(),
        created: Some(today_utc()),
        publish: false,
        extra: Vec::new(),
    });

    // One evidence block per key: a cited paragraph with a stable anchor,
    // ready to be targeted by argument relations (plan §2.4).
    let mut body = String::new();
    for (i, key) in cites.iter().enumerate() {
        body.push_str(&format!(
            "\nTODO: summarize the evidence in [@{key}]. #anchor evidence-{}\n",
            i + 1
        ));
    }
    create_note(&id, &meta, &body, vault, format)
}

fn create_argument(title: &str, to: &[String], vault: Option<PathBuf>, format: &str) -> ExitCode {
    let Some(id) = slugify(title) else {
        return fail(format_args!("cannot derive a slug id from `{title}`"));
    };
    let mut relations = Vec::new();
    for spec in to {
        match parse_relation_spec(spec) {
            Ok(rel) => relations.push(rel),
            Err(message) => return fail(format_args!("{message}")),
        }
    }

    let meta = NoteMeta::Argument(ArgumentMeta {
        id: Some(NoteId::new(id.as_str())),
        uuid: Some(new_uuid()),
        title: Some(title.to_owned()),
        form: ArgumentForm::Claim,
        thesis: None,
        relations,
        status: Some(ArgumentStatus::Draft),
        created: Some(today_utc()),
        publish: false,
        extra: Vec::new(),
    });
    create_note(&id, &meta, "", vault, format)
}

/// Parse one `--to ID[#anchor]:stance[:op]` specification.
fn parse_relation_spec(spec: &str) -> Result<RelationDecl, String> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (to, stance, op) = match parts.as_slice() {
        [to, stance] => (*to, *stance, None),
        [to, stance, op] => (*to, *stance, Some(*op)),
        _ => {
            return Err(format!("`--to {spec}` is not of the form ID:stance[:op]"));
        }
    };
    let (target_id, target_anchor) = split_target(to);
    if !is_slug(&target_id) || target_anchor.as_deref().is_some_and(|a| !is_slug(a)) {
        return Err(format!(
            "`--to {spec}`: target `{to}` is not a slug-shaped id (with optional #anchor)"
        ));
    }
    let stance = Stance::parse(stance).ok_or_else(|| {
        format!(
            "`--to {spec}`: invalid stance `{stance}` (expected one of: {})",
            Stance::ALL.join(", ")
        )
    })?;
    let op = op
        .map(|op| {
            BloomOp::parse(op).ok_or_else(|| {
                format!(
                    "`--to {spec}`: invalid op `{op}` (expected one of: {})",
                    BloomOp::ALL.join(", ")
                )
            })
        })
        .transpose()?;
    Ok(RelationDecl {
        to: to.to_owned(),
        target_id,
        target_anchor,
        stance,
        op,
        span: morg_parser::Span::new(0, 0, 1, 1),
    })
}

/// Serialize, verify the round-trip, check id/file collisions, and write
/// `<vault>/<id>.md`.
fn create_note(
    id: &str,
    meta: &NoteMeta,
    body: &str,
    vault: Option<PathBuf>,
    format: &str,
) -> ExitCode {
    let vault = vault.unwrap_or_else(|| PathBuf::from("."));
    if !vault.is_dir() {
        return fail(format_args!(
            "vault `{}` is not a directory",
            vault.display()
        ));
    }

    // Scan the declared ids (and kinds, for relation checks) in the vault.
    let existing = match scan_vault(&vault) {
        Ok(existing) => existing,
        Err(message) => return fail(format_args!("{message}")),
    };
    if let Some((path, _)) = existing.get(id) {
        return fail(format_args!(
            "id `{id}` is already declared in {}",
            path.display()
        ));
    }

    // An argument must not target a source (sta::relation-target); an
    // unresolved target is only a warning — the topic may be created next.
    if let NoteMeta::Argument(m) = meta {
        for rel in &m.relations {
            match existing.get(rel.target_id.as_str()) {
                Some((path, Some(NoteKind::Source))) => {
                    return fail(format_args!(
                        "relation target `{}` is a source note ({}); arguments should go \
                         through topics (sta::relation-target)",
                        rel.target_id,
                        path.display()
                    ));
                }
                Some(_) => {}
                None => {
                    eprintln!(
                        "morog research: relation target `{}` does not resolve to any note yet",
                        rel.to
                    );
                }
            }
        }
    }

    let content = format!("{}{body}", serialize_frontmatter(meta));

    // The round-trip guarantee: what we are about to write must come back
    // from morg-parser + parse_note_meta with zero MetaErrors and the same
    // typed value (spans aside — the input meta carries no real spans yet).
    let parsed = morg_parser::parse_document(&content);
    let (reparsed, errors) = parse_note_meta(parsed.document.frontmatter.as_ref());
    if !errors.is_empty() || normalize_spans(reparsed) != normalize_spans(meta.clone()) {
        return fail(format_args!(
            "internal error: serialized frontmatter does not round-trip cleanly ({errors:?})"
        ));
    }

    let path = vault.join(format!("{id}.md"));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    let result = open
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, content.as_bytes()));
    if let Err(err) = result {
        return fail(format_args!("cannot create {}: {err}", path.display()));
    }

    let kind = meta.kind().map_or("note", NoteKind::as_str);
    if format == "json" {
        println!(
            "{{\"file\": \"{}\", \"id\": \"{}\", \"kind\": \"{kind}\"}}",
            crate::json::escape(&path.display().to_string()),
            crate::json::escape(id)
        );
    } else {
        println!("created {kind} {}", path.display());
    }
    ExitCode::SUCCESS
}

/// Walk the vault and map every declared note id to its file and kind.
fn scan_vault(vault: &Path) -> Result<IdMap, String> {
    let mut files = Vec::new();
    crate::lint::collect_notes(vault, &mut files);
    let mut out = IdMap::new();
    for path in files {
        let source = std::fs::read_to_string(&path)
            .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
        let parsed = morg_parser::parse_document(&source);
        let (meta, _) = parse_note_meta(parsed.document.frontmatter.as_ref());
        if let Some(id) = meta.id() {
            out.entry(id.as_str().to_owned())
                .or_insert_with(|| (path.clone(), meta.kind()));
        }
    }
    Ok(out)
}

type IdMap = std::collections::HashMap<String, (PathBuf, Option<NoteKind>)>;

/// Zero the source spans inside a [`NoteMeta`] so two metas can be compared
/// for semantic equality regardless of where their text sat.
fn normalize_spans(mut meta: NoteMeta) -> NoteMeta {
    let zero = morg_parser::Span::new(0, 0, 1, 1);
    match &mut meta {
        NoteMeta::Argument(m) => {
            for rel in &mut m.relations {
                rel.span = zero;
            }
        }
        NoteMeta::Topic(m) => {
            for rel in &mut m.related {
                rel.span = zero;
            }
        }
        _ => {}
    }
    meta
}

// ---------------------------------------------------------------------------
// Frontmatter serialization

/// Emit YAML frontmatter from a typed [`NoteMeta`], in the field order the
/// fixture vault uses. `extra` is not serialized — creation never sets it.
fn serialize_frontmatter(meta: &NoteMeta) -> String {
    let mut out = String::from("---\n");
    let push = |out: &mut String, key: &str, value: &str| {
        out.push_str(key);
        out.push_str(": ");
        out.push_str(value);
        out.push('\n');
    };
    let push_quoted = |out: &mut String, key: &str, value: &str| push(out, key, &quote_yaml(value));

    match meta {
        NoteMeta::Plain => {}
        NoteMeta::Source(m) => {
            if let Some(id) = &m.id {
                push(&mut out, "id", id.as_str());
            }
            if let Some(uuid) = &m.uuid {
                push(&mut out, "uuid", &uuid.to_string());
            }
            push(&mut out, "kind", "source");
            if let Some(title) = &m.title {
                push_quoted(&mut out, "title", title);
            }
            if let Some(status) = m.status {
                push(&mut out, "status", status.as_str());
            }
            if let Some(created) = &m.created {
                push(&mut out, "created", created);
            }
            if m.publish {
                push(&mut out, "publish", "true");
            }
        }
        NoteMeta::Topic(m) => {
            if let Some(id) = &m.id {
                push(&mut out, "id", id.as_str());
            }
            if let Some(uuid) = &m.uuid {
                push(&mut out, "uuid", &uuid.to_string());
            }
            push(&mut out, "kind", "topic");
            if let Some(title) = &m.title {
                push_quoted(&mut out, "title", title);
            }
            if !m.aliases.is_empty() {
                let items: Vec<String> = m.aliases.iter().map(|a| quote_yaml(a)).collect();
                push(&mut out, "aliases", &format!("[{}]", items.join(", ")));
            }
            if !m.related.is_empty() {
                let items: Vec<&str> = m.related.iter().map(|r| r.to.as_str()).collect();
                push(&mut out, "related", &format!("[{}]", items.join(", ")));
            }
            if let Some(created) = &m.created {
                push(&mut out, "created", created);
            }
            if m.publish {
                push(&mut out, "publish", "true");
            }
        }
        NoteMeta::Argument(m) => {
            if let Some(id) = &m.id {
                push(&mut out, "id", id.as_str());
            }
            if let Some(uuid) = &m.uuid {
                push(&mut out, "uuid", &uuid.to_string());
            }
            push(&mut out, "kind", "argument");
            if let Some(title) = &m.title {
                push_quoted(&mut out, "title", title);
            }
            push(&mut out, "form", m.form.as_str());
            if let Some(thesis) = &m.thesis {
                push_quoted(&mut out, "thesis", thesis);
            }
            if !m.relations.is_empty() {
                out.push_str("relations:\n");
                for rel in &m.relations {
                    out.push_str(&format!(
                        "  - {{ to: {}, stance: {}",
                        rel.to,
                        rel.stance.as_str()
                    ));
                    if let Some(op) = rel.op {
                        out.push_str(&format!(", op: {}", op.as_str()));
                    }
                    out.push_str(" }\n");
                }
            }
            if let Some(status) = m.status {
                push(&mut out, "status", status.as_str());
            }
            if let Some(created) = &m.created {
                push(&mut out, "created", created);
            }
            push(
                &mut out,
                "publish",
                if m.publish { "true" } else { "false" },
            );
        }
    }
    out.push_str("---\n");
    out
}

/// Double-quote a YAML scalar, escaping backslashes and quotes.
fn quote_yaml(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// Identity helpers

/// A fresh note uuid. Version 7: time-ordered (new notes sort by creation
/// in the index and on disk listings) while staying globally unique, and it
/// matches the v7 uuids already used across the fixture vault.
fn new_uuid() -> NoteUuid {
    NoteUuid::new(uuid::Uuid::now_v7())
}

/// Slugify a title into a note id: ASCII alphanumerics (lowercased) and
/// `_` survive, every other run of characters collapses to one `-`.
/// Returns `None` when nothing slug-shaped remains.
fn slugify(title: &str) -> Option<String> {
    let mut out = String::new();
    let mut pending = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            if pending && !out.is_empty() {
                out.push('-');
            }
            pending = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending = true;
        }
    }
    // A leading `_` is not a valid slug start; trim it.
    let out = out.trim_start_matches('_').to_string();
    if out.is_empty() { None } else { Some(out) }
}

/// Today's date (UTC) as `YYYY-MM-DD`, without a date-time dependency.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Gregorian date from days since the Unix epoch (Howard Hinnant's
/// `civil_from_days` algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(m <= 2), m, d)
}

// ---------------------------------------------------------------------------
// Listing

fn list(vault: Option<PathBuf>, format: &str) -> ExitCode {
    let vault = vault.unwrap_or_else(|| PathBuf::from("."));
    if !vault.is_dir() {
        return fail(format_args!(
            "vault `{}` is not a directory",
            vault.display()
        ));
    }
    let mut files = Vec::new();
    crate::lint::collect_notes(&vault, &mut files);
    files.sort();

    struct Row {
        file: PathBuf,
        kind: NoteKind,
        status: Option<&'static str>,
        id: String,
        title: String,
    }
    let mut rows = Vec::new();
    for path in files {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(err) => {
                eprintln!("morog research: cannot read {}: {err}", path.display());
                continue;
            }
        };
        let parsed = morg_parser::parse_document(&source);
        let (meta, _) = parse_note_meta(parsed.document.frontmatter.as_ref());
        let Some(kind) = meta.kind() else {
            continue; // plain morg note
        };
        let (status, title) = match &meta {
            NoteMeta::Source(m) => (m.status.map(SourceStatus::as_str), m.title.clone()),
            NoteMeta::Topic(m) => (None, m.title.clone()),
            NoteMeta::Argument(m) => (m.status.map(ArgumentStatus::as_str), m.title.clone()),
            NoteMeta::Plain => unreachable!(),
        };
        rows.push(Row {
            file: path,
            kind,
            status,
            id: meta
                .id()
                .map(|id| id.as_str().to_owned())
                .unwrap_or_default(),
            title: title.unwrap_or_default(),
        });
    }
    rows.sort_by(|a, b| (a.kind.as_str(), &a.id).cmp(&(b.kind.as_str(), &b.id)));

    if format == "json" {
        let mut out = String::from("[");
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "\n  {{\"file\": \"{}\", \"id\": \"{}\", \"kind\": \"{}\", \"status\": {}, \"title\": \"{}\"}}",
                crate::json::escape(&row.file.display().to_string()),
                crate::json::escape(&row.id),
                row.kind.as_str(),
                row.status
                    .map_or("null".to_owned(), |s| format!("\"{s}\"")),
                crate::json::escape(&row.title),
            ));
        }
        if !rows.is_empty() {
            out.push('\n');
        }
        out.push_str("]\n");
        print!("{out}");
    } else {
        let id_width = rows.iter().map(|r| r.id.len()).max().unwrap_or(0);
        for row in &rows {
            println!(
                "{:<8}  {:<10}  {:<id_width$}  {}",
                row.kind.as_str(),
                row.status.unwrap_or("-"),
                row.id,
                row.title
            );
        }
        let count = |kind: NoteKind| rows.iter().filter(|r| r.kind == kind).count();
        println!(
            "{} note(s): {} source, {} topic, {} argument",
            rows.len(),
            count(NoteKind::Source),
            count(NoteKind::Topic),
            count(NoteKind::Argument)
        );
    }
    ExitCode::SUCCESS
}

fn fail(message: std::fmt::Arguments<'_>) -> ExitCode {
    eprintln!("morog research: {message}");
    ExitCode::FAILURE
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_shapes() {
        assert_eq!(
            slugify("Specific Leaf Area").as_deref(),
            Some("specific-leaf-area")
        );
        assert_eq!(
            slugify("  Leading & trailing!  ").as_deref(),
            Some("leading-trailing")
        );
        assert_eq!(
            slugify("paszke_pytorch_2019").as_deref(),
            Some("paszke_pytorch_2019")
        );
        assert_eq!(slugify("Café living").as_deref(), Some("caf-living"));
        assert_eq!(slugify("A--b__c").as_deref(), Some("a-b__c"));
        assert_eq!(slugify("_hidden").as_deref(), Some("hidden"));
        assert_eq!(slugify("???").as_deref(), None);
        assert_eq!(slugify("").as_deref(), None);
        // Every produced id satisfies the sta::id slug lint.
        for title in ["Specific Leaf Area", "Café living", "a  b", "42 things"] {
            assert!(is_slug(&slugify(title).unwrap()), "{title}");
        }
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // leap year
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_726), (2026, 9, 30));
        let today = today_utc();
        assert_eq!(today.len(), 10);
        assert_eq!(today.as_bytes()[4], b'-');
    }

    /// Serialize → parse → retype must yield zero MetaErrors and the exact
    /// same typed struct, for every creation path.
    fn assert_round_trip(meta: &NoteMeta) {
        let text = serialize_frontmatter(meta);
        let parsed = morg_parser::parse_document(&text);
        let (reparsed, errors) = parse_note_meta(parsed.document.frontmatter.as_ref());
        assert!(errors.is_empty(), "{errors:?}\n{text}");
        assert_eq!(&reparsed, meta, "{text}");
    }

    fn uuid_fixture() -> NoteUuid {
        "0192f3c4-7a11-7bbb-8f55-111111111111".parse().unwrap()
    }

    #[test]
    fn source_round_trips() {
        for title in [
            None,
            Some("PyTorch: An \"Imperative\" Style \\ More".to_owned()),
        ] {
            assert_round_trip(&NoteMeta::Source(SourceMeta {
                id: Some(NoteId::new("paszke_pytorch_2019")),
                uuid: Some(uuid_fixture()),
                title,
                status: Some(SourceStatus::Unread),
                created: Some("2026-09-30".to_owned()),
                publish: false,
                extra: Vec::new(),
            }));
        }
    }

    #[test]
    fn topic_round_trips() {
        assert_round_trip(&NoteMeta::Topic(TopicMeta {
            id: Some(NoteId::new("specific-leaf-area")),
            uuid: Some(uuid_fixture()),
            title: Some("Specific leaf area".to_owned()),
            aliases: Vec::new(),
            related: Vec::new(),
            created: Some("2026-09-30".to_owned()),
            publish: false,
            extra: Vec::new(),
        }));
    }

    #[test]
    fn argument_round_trips_with_relations() {
        let relations = vec![
            parse_relation_spec("specific-leaf-area#evidence-1:supports:analyze").unwrap(),
            parse_relation_spec("leaf-mass-ratio:qualifies").unwrap(),
        ];
        // Round-trip equality includes the relation spans, so align them
        // with where the serializer will put the `to:` text.
        let meta = NoteMeta::Argument(ArgumentMeta {
            id: Some(NoteId::new("an-argument")),
            uuid: Some(uuid_fixture()),
            title: Some("Allocation, not economics".to_owned()),
            form: ArgumentForm::Claim,
            thesis: None,
            relations,
            status: Some(ArgumentStatus::Draft),
            created: Some("2026-09-30".to_owned()),
            publish: false,
            extra: Vec::new(),
        });
        let text = serialize_frontmatter(&meta);
        let parsed = morg_parser::parse_document(&text);
        let (reparsed, errors) = parse_note_meta(parsed.document.frontmatter.as_ref());
        assert!(errors.is_empty(), "{errors:?}\n{text}");
        let NoteMeta::Argument(m) = &reparsed else {
            panic!("expected argument");
        };
        assert_eq!(m.relations.len(), 2);
        assert_eq!(m.relations[0].target_anchor.as_deref(), Some("evidence-1"));
        assert_eq!(m.relations[0].op, Some(BloomOp::Analyze));
        assert_eq!(m.relations[1].stance, Stance::Qualifies);
        assert_eq!(m.relations[1].op, None);
        // The reparsed span slices the target text exactly.
        let span = m.relations[1].span;
        assert_eq!(&text[span.start..span.end], "leaf-mass-ratio");
    }

    #[test]
    fn relation_spec_validation() {
        assert!(parse_relation_spec("a:supports").is_ok());
        assert!(parse_relation_spec("a#b:opposes:recall").is_ok());
        let err = parse_relation_spec("a:maybe").unwrap_err();
        assert!(err.contains("supports, opposes, qualifies"), "{err}");
        let err = parse_relation_spec("a:supports:ponder").unwrap_err();
        assert!(err.contains("recall, explain"), "{err}");
        assert!(parse_relation_spec("a").is_err());
        assert!(parse_relation_spec("a:supports:analyze:extra").is_err());
        assert!(parse_relation_spec("no slug:supports").is_err());
        assert!(parse_relation_spec("a#bad anchor:supports").is_err());
    }
}

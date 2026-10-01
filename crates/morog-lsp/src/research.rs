//! Minimal note creation for the code actions and the `morog.research.new`
//! command (plan §5.6: "the same path as `morog research`").
//!
//! This **replicates** the serialization half of `morog research`
//! (`crates/morog/src/research.rs`): the `morog` crate is a binary, so its
//! research module is not cleanly importable from here. The replica keeps
//! the same field order and the same guarantee — the emitted frontmatter
//! round-trips through `morg-parser` + `parse_note_meta` with zero
//! `MetaError`s (asserted in the tests below).

use morog_sema::NoteKind;

/// Frontmatter for a new source note anchoring the bibrat entry `key`.
pub(crate) fn source_note(key: &str, title: Option<&str>) -> String {
    let mut out = header(key, NoteKind::Source);
    if let Some(title) = title {
        push(&mut out, "title", &quote_yaml(title));
    }
    push(&mut out, "status", "unread");
    push(&mut out, "created", &today_utc());
    out.push_str("---\n");
    out
}

/// Frontmatter for a new topic note.
pub(crate) fn topic_note(id: &str, title: &str) -> String {
    let mut out = header(id, NoteKind::Topic);
    push(&mut out, "title", &quote_yaml(title));
    push(&mut out, "created", &today_utc());
    out.push_str("---\n");
    out
}

/// Frontmatter for a new argument note.
pub(crate) fn argument_note(id: &str, title: &str) -> String {
    let mut out = header(id, NoteKind::Argument);
    push(&mut out, "title", &quote_yaml(title));
    push(&mut out, "form", "claim");
    push(&mut out, "status", "draft");
    push(&mut out, "created", &today_utc());
    push(&mut out, "publish", "false");
    out.push_str("---\n");
    out
}

fn header(id: &str, kind: NoteKind) -> String {
    let mut out = String::from("---\n");
    push(&mut out, "id", id);
    push(&mut out, "uuid", &uuid::Uuid::now_v7().to_string());
    push(&mut out, "kind", kind.as_str());
    out
}

fn push(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(value);
    out.push('\n');
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

/// Slugify a title into a note id (the `morog research` rules): ASCII
/// alphanumerics (lowercased) and `_` survive, every other run collapses to
/// one `-`. `None` when nothing slug-shaped remains.
pub(crate) fn slugify(title: &str) -> Option<String> {
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

#[cfg(test)]
mod tests {
    use morog_sema::{NoteMeta, parse_note_meta};

    use super::*;

    /// The `morog research` guarantee: emitted text parses back with zero
    /// `MetaError`s and the intended identity.
    fn assert_round_trip(text: &str, id: &str, kind: NoteKind) {
        let parsed = morg_parser::parse_document(text);
        let (meta, errors) = parse_note_meta(parsed.document.frontmatter.as_ref());
        assert!(errors.is_empty(), "{errors:?}\n{text}");
        assert_eq!(meta.kind(), Some(kind), "{text}");
        assert_eq!(meta.id().unwrap().as_str(), id, "{text}");
        if let NoteMeta::Source(m) = &meta {
            assert_eq!(m.status, Some(morog_sema::SourceStatus::Unread));
        }
    }

    #[test]
    fn created_notes_round_trip() {
        assert_round_trip(
            &source_note("paszke_pytorch_2019", Some("PyTorch \"fast\"")),
            "paszke_pytorch_2019",
            NoteKind::Source,
        );
        assert_round_trip(&source_note("bare_key", None), "bare_key", NoteKind::Source);
        assert_round_trip(
            &topic_note("specific-leaf-area", "Specific leaf area"),
            "specific-leaf-area",
            NoteKind::Topic,
        );
        assert_round_trip(
            &argument_note("an-argument", "It is all allocation"),
            "an-argument",
            NoteKind::Argument,
        );
    }

    #[test]
    fn slugify_matches_morog_research() {
        assert_eq!(
            slugify("Specific Leaf Area").as_deref(),
            Some("specific-leaf-area")
        );
        assert_eq!(slugify("Café living").as_deref(), Some("caf-living"));
        assert_eq!(slugify("_hidden").as_deref(), Some("hidden"));
        assert_eq!(slugify("???"), None);
        for title in ["Specific Leaf Area", "a  b", "42 things"] {
            assert!(morog_sema::is_slug(&slugify(title).unwrap()), "{title}");
        }
    }
}

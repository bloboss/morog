//! `morog tags <name>` — tabulate a custom tag's occurrences (plan §10 T1).
//!
//! Occurrences come straight **from the index** (the `blocks.customs`
//! column; like `morog graph`, an existing index is required — never build
//! silently). The column set and order come from the resolved `[tags]`
//! declaration: `TagTable::group_names` yields the pattern's named capture
//! groups in pattern order, or the single `kind` shorthand name. Rows whose
//! argument did not match the declared pattern/kind carry no fields and are
//! flagged in the `mismatch` column (`shape_mismatch` in `--format json`).

use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use morog_index::{QueryOutput, TagOccurrence, Value};

pub fn run(name: &str, vault: Option<PathBuf>, format: &str) -> ExitCode {
    match run_inner(name, vault, format) {
        Ok(rendered) => {
            print!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("morog tags: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(name: &str, vault: Option<PathBuf>, format: &str) -> Result<String, String> {
    let vault = vault.unwrap_or_else(|| PathBuf::from("."));
    let vault = vault
        .canonicalize()
        .map_err(|e| format!("cannot open vault {}: {e}", vault.display()))?;

    // The resolved declaration defines the columns; an undeclared name has
    // no field structure to tabulate (and its occurrences, if any, are
    // stale rows from an older table).
    let config = crate::config::load(&vault)?;
    let groups = config.tag_table().group_names(name).ok_or_else(|| {
        format!(
            "tag `{name}` has no [tags] declaration \
             (checked the vault .morog.toml and the morog and morg configs)"
        )
    })?;

    let db_path = morog_index::cache_db_path(&vault);
    let occurrences = morog_index::tag_occurrences(&db_path, name).map_err(|e| e.to_string())?;

    Ok(match format {
        "json" => render_json(&occurrences, &groups),
        _ => render_text(&occurrences, &groups),
    })
}

/// The value an occurrence extracted for one named group, if the group
/// matched (fields are empty on shape mismatch, and optional groups may not
/// participate in a match).
fn field<'a>(occurrence: &'a TagOccurrence, group: &str) -> Option<&'a str> {
    occurrence
        .fields
        .iter()
        .find(|(name, _)| name == group)
        .map(|(_, value)| value.as_str())
}

/// `file  line  <group columns>  mismatch`, through the `morog query`
/// padded-table renderer.
fn render_text(occurrences: &[TagOccurrence], groups: &[String]) -> String {
    let mut columns = vec!["file".to_owned(), "line".to_owned()];
    columns.extend(groups.iter().cloned());
    columns.push("mismatch".to_owned());

    let rows = occurrences
        .iter()
        .map(|occurrence| {
            let mut row = vec![
                Value::Text(occurrence.path.clone()),
                Value::Integer(i64::from(occurrence.line)),
            ];
            row.extend(groups.iter().map(|group| {
                field(occurrence, group).map_or(Value::Null, |value| Value::Text(value.to_owned()))
            }));
            row.push(if occurrence.shape_mismatch {
                Value::Text("yes".to_owned())
            } else {
                Value::Null
            });
            row
        })
        .collect();

    crate::query::render_text(&QueryOutput { columns, rows })
}

/// A JSON array of occurrence objects; `fields` keeps the group order and
/// carries `null` for groups the argument did not yield.
fn render_json(occurrences: &[TagOccurrence], groups: &[String]) -> String {
    let mut text = String::from("[");
    for (i, occurrence) in occurrences.iter().enumerate() {
        if i > 0 {
            text.push(',');
        }
        let _ = write!(
            text,
            "\n  {{\"file\": \"{}\", \"line\": {}, \"fields\": {{",
            crate::json::escape(&occurrence.path),
            occurrence.line
        );
        for (g, group) in groups.iter().enumerate() {
            if g > 0 {
                text.push_str(", ");
            }
            let _ = write!(text, "\"{}\": ", crate::json::escape(group));
            match field(occurrence, group) {
                Some(value) => {
                    let _ = write!(text, "\"{}\"", crate::json::escape(value));
                }
                None => text.push_str("null"),
            }
        }
        let _ = write!(
            text,
            "}}, \"shape_mismatch\": {}}}",
            occurrence.shape_mismatch
        );
    }
    if !occurrences.is_empty() {
        text.push('\n');
    }
    text.push_str("]\n");
    text
}

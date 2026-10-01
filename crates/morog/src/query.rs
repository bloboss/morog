//! `morog query` — read-only SQL over the index; the `v_*` views are the
//! public API (plan §3.3).

use std::path::PathBuf;
use std::process::ExitCode;

use morog_index::{QueryOutput, Value};

pub fn run(sql: &str, vault: Option<PathBuf>, format: &str) -> ExitCode {
    let vault = vault.unwrap_or_else(|| PathBuf::from("."));
    let vault = match vault.canonicalize() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("morog query: cannot open vault {}: {e}", vault.display());
            return ExitCode::FAILURE;
        }
    };
    let db_path = morog_index::cache_db_path(&vault);
    match morog_index::run_query(&db_path, sql) {
        Ok(out) => {
            match format {
                "json" => print!("{}", render_json(&out)),
                _ => print!("{}", render_text(&out)),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("morog query: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cell_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(n) => n.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(t) => t.clone(),
        Value::Blob(b) => b.iter().map(|byte| format!("{byte:02x}")).collect(),
    }
}

/// Columns padded to their widest cell, separated by two spaces. Shared
/// with `morog tags`, which renders its occurrence table through the same
/// shape.
pub(crate) fn render_text(out: &QueryOutput) -> String {
    use std::fmt::Write as _;

    let mut table: Vec<Vec<String>> = vec![out.columns.clone()];
    table.extend(
        out.rows
            .iter()
            .map(|row| row.iter().map(cell_text).collect()),
    );

    let mut widths = vec![0usize; out.columns.len()];
    for row in &table {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut text = String::new();
    for row in &table {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            let _ = write!(line, "{cell:<width$}", width = widths[i]);
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    let _ = writeln!(
        text,
        "({} row{})",
        out.rows.len(),
        if out.rows.len() == 1 { "" } else { "s" }
    );
    text
}

/// A JSON array of row objects keyed by column name.
fn render_json(out: &QueryOutput) -> String {
    use std::fmt::Write as _;

    let mut text = String::from("[");
    for (r, row) in out.rows.iter().enumerate() {
        if r > 0 {
            text.push(',');
        }
        text.push_str("\n  {");
        for (c, cell) in row.iter().enumerate() {
            if c > 0 {
                text.push_str(", ");
            }
            let _ = write!(text, "\"{}\": ", crate::json::escape(&out.columns[c]));
            match cell {
                Value::Null => text.push_str("null"),
                Value::Integer(n) => {
                    let _ = write!(text, "{n}");
                }
                Value::Real(f) if f.is_finite() => {
                    let _ = write!(text, "{f}");
                }
                Value::Real(_) => text.push_str("null"),
                Value::Text(t) => {
                    let _ = write!(text, "\"{}\"", crate::json::escape(t));
                }
                Value::Blob(b) => {
                    let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
                    let _ = write!(text, "\"{hex}\"");
                }
            }
        }
        text.push('}');
    }
    if !out.rows.is_empty() {
        text.push('\n');
    }
    text.push_str("]\n");
    text
}

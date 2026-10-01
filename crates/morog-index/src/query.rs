//! Read-only SQL access for `morog query` (plan §3.3).
//!
//! The views are the public query API; this module only enforces that the
//! access is read-only: the connection is opened `SQLITE_OPEN_READ_ONLY`
//! with `PRAGMA query_only`, and the statement itself must be a single
//! `SELECT` (or `WITH … SELECT`) that SQLite classifies as read-only.

use std::path::Path;

use rusqlite::types::ValueRef;

use crate::{Error, schema};

/// One cell of a result row.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A completed query: column names plus row-major cells.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryOutput {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Run one `SELECT` against the index database, read-only.
pub fn run_query(db_path: &Path, sql: &str) -> Result<QueryOutput, Error> {
    if !db_path.exists() {
        return Err(Error::MissingIndex(db_path.to_path_buf()));
    }
    let conn = schema::open_ro(db_path)?;

    let head = sql.trim_start();
    let is_select = ["SELECT", "WITH"]
        .iter()
        .any(|kw| head.len() >= kw.len() && head[..kw.len()].eq_ignore_ascii_case(kw));
    if !is_select || has_second_statement(sql) {
        return Err(Error::NotSelect);
    }

    let mut stmt = conn.prepare(sql)?;
    // Catches `WITH … INSERT` and friends that the prefix check lets by.
    if !stmt.readonly() {
        return Err(Error::NotSelect);
    }

    let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let n = columns.len();
    let rows = stmt
        .query_map([], |row| {
            let mut cells = Vec::with_capacity(n);
            for i in 0..n {
                cells.push(match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(v) => Value::Integer(v),
                    ValueRef::Real(v) => Value::Real(v),
                    ValueRef::Text(v) => Value::Text(String::from_utf8_lossy(v).into_owned()),
                    ValueRef::Blob(v) => Value::Blob(v.to_vec()),
                });
            }
            Ok(cells)
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(QueryOutput { columns, rows })
}

/// Whether `sql` contains a second statement after a top-level `;`.
///
/// SQLite's prepare API silently stops at the first statement, which would
/// let `SELECT 1; DELETE …` pass the read-only checks by never looking at
/// the tail. Semicolons inside string literals, quoted identifiers, and
/// comments do not count.
fn has_second_statement(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut past_semicolon = false;
    while i < bytes.len() {
        let rest = &sql[i..];
        let (skip, is_code) = match bytes[i] {
            b'\'' | b'"' | b'`' => (quoted_len(rest, bytes[i]), true),
            b'[' => (rest.find(']').map_or(rest.len(), |p| p + 1), true),
            b'-' if rest.starts_with("--") => {
                (rest.find('\n').map_or(rest.len(), |p| p + 1), false)
            }
            b'/' if rest.starts_with("/*") => {
                (rest.find("*/").map_or(rest.len(), |p| p + 2), false)
            }
            b';' => {
                past_semicolon = true;
                (1, false)
            }
            c => (1, !c.is_ascii_whitespace()),
        };
        if past_semicolon && is_code {
            return true;
        }
        i += skip.max(1);
    }
    false
}

/// Length of a quote-delimited token starting at `s[0] == quote`, honoring
/// SQL's doubled-quote escape.
fn quoted_len(s: &str, quote: u8) -> usize {
    let bytes = s.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    s.len()
}

#[cfg(test)]
mod tests {
    use super::has_second_statement;

    #[test]
    fn second_statement_detection() {
        assert!(!has_second_statement("SELECT 1"));
        assert!(!has_second_statement("SELECT 1;"));
        assert!(!has_second_statement("SELECT 1 ;  \n-- trailing comment"));
        assert!(!has_second_statement("SELECT ';' FROM notes"));
        assert!(!has_second_statement("SELECT \"a;b\" FROM notes"));
        assert!(!has_second_statement("SELECT 1 /* ; still fine ; */"));
        assert!(!has_second_statement("SELECT 'it''s; quoted'"));
        assert!(has_second_statement("SELECT 1; DELETE FROM files"));
        assert!(has_second_statement("SELECT 1;\n-- x\nDROP TABLE notes"));
    }
}

//! Minimal JSON string escaping shared by the `--format json` renderers.

use std::fmt::Write as _;

/// Escape a string for inclusion in a JSON string literal.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn json_escaping() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(escape("line\nbreak"), "line\\nbreak");
        assert_eq!(escape("caf\u{e9} \u{1f980}"), "caf\u{e9} \u{1f980}");
        assert_eq!(escape("\u{1}"), "\\u0001");
    }
}

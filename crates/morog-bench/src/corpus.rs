//! Corpus loading: reads the downloaded plain-text books, strips Project
//! Gutenberg boilerplate, and exposes a flat, sanitized word stream.

use std::fs;
use std::io;
use std::path::Path;

/// A flat stream of prose words backed by one owned buffer.
pub struct Corpus {
    text: String,
    words: Vec<(u32, u32)>,
}

impl Corpus {
    /// Load every `*.txt` file under `dir` (sorted by name for determinism).
    pub fn load(dir: &Path) -> io::Result<Self> {
        let mut paths: Vec<_> = fs::read_dir(dir)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "txt"))
            .collect();
        paths.sort();
        if paths.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no .txt corpus files in {}", dir.display()),
            ));
        }

        let mut text = String::new();
        for path in &paths {
            let raw = fs::read_to_string(path)?;
            text.push_str(strip_gutenberg(&raw));
            text.push(' ');
        }
        Ok(Self::from_text(&text))
    }

    /// Build a corpus from raw prose (also the test entry point).
    pub fn from_text(text: &str) -> Self {
        let mut words = Vec::new();
        let mut sanitized = String::with_capacity(text.len());
        for token in text.split_whitespace() {
            let start = sanitized.len();
            for ch in token.chars() {
                if keep_char(ch) {
                    sanitized.push(ch);
                }
            }
            let end = sanitized.len();
            if end > start {
                words.push((start as u32, end as u32));
                sanitized.push(' ');
            }
        }

        Self {
            text: sanitized,
            words,
        }
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn word(&self, i: usize) -> &str {
        let (s, e) = self.words[i % self.words.len()];
        &self.text[s as usize..e as usize]
    }
}

/// Keep prose punctuation; drop anything that is morg/YAML markup
/// (brackets, hashes, quotes, emphasis markers, backticks, ...).
fn keep_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '\'' | '.' | ',' | ';' | '!' | '?' | '-')
}

/// Cut the Project Gutenberg header/footer when the markers are present.
fn strip_gutenberg(raw: &str) -> &str {
    let body_start = raw
        .find("*** START OF")
        .and_then(|i| raw[i..].find('\n').map(|nl| i + nl + 1))
        .unwrap_or(0);
    let body_end = raw.find("*** END OF").unwrap_or(raw.len());
    &raw[body_start..body_end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_gutenberg_markers() {
        let raw = "junk header\n*** START OF THE PROJECT GUTENBERG EBOOK X ***\nreal text here\n*** END OF THE PROJECT GUTENBERG EBOOK X ***\njunk footer";
        assert_eq!(strip_gutenberg(raw).trim(), "real text here");
    }

    #[test]
    fn sanitizer_drops_markup_chars() {
        assert!(!keep_char('['));
        assert!(!keep_char('#'));
        assert!(!keep_char('"'));
        assert!(!keep_char('`'));
        assert!(keep_char('é'));
        assert!(keep_char('\''));
    }
}

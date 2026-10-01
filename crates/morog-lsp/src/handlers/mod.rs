//! One module per LSP method (plan §5.1), plus the plumbing they share.
//!
//! Handlers run on the rayon pool against a [`Snapshot`]; they never touch
//! the vfs. Text comes from the snapshot's `FileAnalysis` when the file was
//! parsed this session (open buffers included), and lazily from disk for
//! facts-only cold-start entries — never a reparse.

pub(crate) mod code_action;
pub(crate) mod completion;
pub(crate) mod document_symbol;
pub(crate) mod goto_definition;
pub(crate) mod hover;
pub(crate) mod inlay_hint;
pub(crate) mod neighbourhood;
pub(crate) mod references;
pub(crate) mod rename;
pub(crate) mod semantic_tokens;
pub(crate) mod workspace_symbol;

use std::collections::BTreeMap;
use std::sync::Arc;

use lsp_server::ErrorCode;
use lsp_types::{
    DocumentChangeOperation, DocumentChanges, Location, OneOf,
    OptionalVersionedTextDocumentIdentifier, TextDocumentEdit, TextDocumentPositionParams,
    TextEdit, Url, WorkspaceEdit,
};
use morg_parser::{LineIndex, Span};

use crate::analysis::FileAnalysis;
use crate::state::Snapshot;
use crate::vfs::FileId;
use crate::{from_proto, to_proto};

/// Handler error surface, matching `GlobalState::dispatch`.
pub(crate) type HandlerError = (ErrorCode, String);

/// A resolved `TextDocumentPositionParams`: the file's analysis and text,
/// and the UTF-8 byte offset of the cursor.
pub(crate) struct FilePosition {
    pub fa: Arc<FileAnalysis>,
    pub text: Arc<str>,
    pub offset: usize,
}

/// Resolve a protocol position. `Ok(None)` means "unknown file or no text":
/// the handler answers null rather than erroring.
pub(crate) fn file_position(
    snapshot: &Snapshot,
    params: &TextDocumentPositionParams,
) -> Option<FilePosition> {
    let path = from_proto::abs_path(&params.text_document.uri)?;
    let file = snapshot.analysis.file_id(&path)?;
    let fa = Arc::clone(snapshot.analysis.file(file)?);
    let text = file_text(snapshot, file)?;
    let index = LineIndex::new(&text);
    let offset = from_proto::offset(&text, &index, params.position, snapshot.encoding);
    Some(FilePosition { fa, text, offset })
}

/// A file's text: the session's parse (overlay or indexer) when present,
/// else a lazy disk read for cold-start facts-only entries.
pub(crate) fn file_text(snapshot: &Snapshot, file: FileId) -> Option<Arc<str>> {
    if let Some(fa) = snapshot.analysis.file(file)
        && let Some(text) = &fa.text
    {
        return Some(Arc::clone(text));
    }
    let path = snapshot.analysis.path(file)?;
    std::fs::read_to_string(path).ok().map(Arc::from)
}

/// A protocol `Location` for a span inside `file`, converting through that
/// file's own text.
pub(crate) fn location(snapshot: &Snapshot, file: FileId, span: Span) -> Option<Location> {
    let uri = file_uri(snapshot, file)?;
    let text = file_text(snapshot, file)?;
    let index = LineIndex::new(&text);
    Some(Location {
        uri,
        range: to_proto::range(&text, &index, span, snapshot.encoding),
    })
}

/// A location pointing at a whole file: its id declaration when it has one,
/// else the start of the file.
pub(crate) fn file_location(snapshot: &Snapshot, file: FileId) -> Option<Location> {
    let span = snapshot
        .analysis
        .file(file)
        .and_then(|fa| fa.facts.note.id_span);
    match span {
        Some(span) => location(snapshot, file, span),
        None => Some(Location {
            uri: file_uri(snapshot, file)?,
            range: lsp_types::Range::default(),
        }),
    }
}

pub(crate) fn file_uri(snapshot: &Snapshot, file: FileId) -> Option<Url> {
    snapshot.analysis.path(file).and_then(|p| to_proto::url(p))
}

/// Case-insensitive subsequence match (`query` must already be lowercased)
/// — the fuzzy matcher shared by workspace symbols and completion
/// (plan §5.6: "matched fuzzily on id, title, and aliases").
pub(crate) fn fuzzy_match(query: &str, candidate: &str) -> bool {
    let mut chars = query.chars();
    let mut needle = match chars.next() {
        Some(c) => c,
        None => return true,
    };
    for c in candidate.to_lowercase().chars() {
        if c == needle {
            match chars.next() {
                Some(next) => needle = next,
                None => return true,
            }
        }
    }
    false
}

/// The block of `facts` declaring `anchor`.
pub(crate) fn anchor_block<'a>(
    fa: &'a FileAnalysis,
    anchor: &str,
) -> Option<&'a morog_sema::BlockFacts> {
    fa.facts
        .blocks
        .iter()
        .find(|block| block.anchor.as_deref() == Some(anchor))
}

/// The first `n` whitespace-separated words, with an ellipsis when cut.
pub(crate) fn first_words(text: &str, n: usize) -> String {
    let mut words = text.split_whitespace();
    let mut out: Vec<&str> = words.by_ref().take(n).collect();
    let cut = words.next().is_some();
    if cut && let Some(last) = out.last_mut() {
        *last = last.trim_end_matches(|c: char| c.is_ascii_punctuation());
    }
    let mut out = out.join(" ");
    if cut {
        out.push('…');
    }
    out
}

/// Where a reference slice mentions its target id: after `(id:` for inline
/// links, at the (possibly quoted) start for frontmatter `to:` values.
/// Shared by rename and the code actions that rewrite relation targets.
pub(crate) fn id_pos_in_ref(slice: &str, target_id: &str) -> Option<usize> {
    if let Some(p) = slice.find("(id:") {
        let pos = p + 4;
        return slice[pos..].starts_with(target_id).then_some(pos);
    }
    let pos = slice.len() - slice.trim_start_matches(['"', '\'']).len();
    slice[pos..].starts_with(target_id).then_some(pos)
}

/// Byte-range edits accumulated per file, converted to protocol
/// `documentChanges` at the end. Shared by rename and the code actions.
#[derive(Default)]
pub(crate) struct Edits {
    by_file: BTreeMap<FileId, Vec<(usize, usize, String)>>,
}

impl Edits {
    pub(crate) fn push(&mut self, file: FileId, start: usize, end: usize, new_text: String) {
        self.by_file
            .entry(file)
            .or_default()
            .push((start, end, new_text));
    }

    /// One `TextDocumentEdit` operation per touched file, edits in document
    /// order.
    pub(crate) fn into_operations(self, snapshot: &Snapshot) -> Vec<DocumentChangeOperation> {
        let mut operations = Vec::with_capacity(self.by_file.len());
        for (file, mut edits) in self.by_file {
            let (Some(uri), Some(text)) = (file_uri(snapshot, file), file_text(snapshot, file))
            else {
                continue;
            };
            let index = LineIndex::new(&text);
            edits.sort_by_key(|&(start, ..)| start);
            edits.dedup();
            let edits = edits
                .into_iter()
                .map(|(start, end, new_text)| {
                    OneOf::Left(TextEdit {
                        range: to_proto::range(
                            &text,
                            &index,
                            Span::new(start, end, 1, 1),
                            snapshot.encoding,
                        ),
                        new_text,
                    })
                })
                .collect();
            operations.push(DocumentChangeOperation::Edit(TextDocumentEdit {
                text_document: OptionalVersionedTextDocumentIdentifier { uri, version: None },
                edits,
            }));
        }
        operations
    }
}

/// A `WorkspaceEdit` from `documentChanges` operations.
pub(crate) fn workspace_edit(operations: Vec<DocumentChangeOperation>) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: None,
        document_changes: Some(DocumentChanges::Operations(operations)),
        change_annotations: None,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Fixture support for handler unit tests: build a snapshot from
    //! in-memory files, with a `$0` cursor marker in one of them
    //! (rust-analyzer style, plan §5.7).

    use std::path::Path;
    use std::sync::Arc;

    use lsp_types::{Position, TextDocumentIdentifier, TextDocumentPositionParams};
    use morg_parser::LineIndex;

    use crate::PositionEncoding;
    use crate::analysis::{Analysis, BibIndex, FileAnalysis};
    use crate::state::Snapshot;
    use crate::vfs::FileId;

    /// Build a snapshot from `(path, text)` fixtures; exactly one text may
    /// carry a `$0` marker. Returns the snapshot and the marker's
    /// `TextDocumentPositionParams`.
    pub(crate) fn snapshot(
        files: &[(&str, &str)],
        bib_keys: &[&str],
    ) -> (Snapshot, TextDocumentPositionParams) {
        let bib = Arc::new(BibIndex::from_entries(bib_keys.iter().map(|&key| {
            let mut entry = morog_bib::BibEntry::minimal(key);
            entry.collection = morog_bib::CollectionPath("fixtures".into());
            entry.dir = Path::new("/bib").join(key);
            entry.meta = Some(morog_bib::BibMeta {
                title: Some(format!("The {key} paper")),
                year: Some(2020),
                doi: None,
            });
            entry
        })));
        let mut analysis = Analysis {
            bib,
            ..Analysis::default()
        };

        let mut marker = None;
        for (i, (path, text)) in files.iter().enumerate() {
            let file = FileId(i as u32);
            let clean;
            let text = if let Some(offset) = text.find("$0") {
                clean = text.replacen("$0", "", 1);
                assert!(marker.is_none(), "two $0 markers");
                let index = LineIndex::new(&clean);
                let position =
                    crate::to_proto::position(&clean, &index, offset, PositionEncoding::Utf8);
                marker = Some((path.to_string(), position));
                clean.as_str()
            } else {
                text
            };
            let path: Arc<Path> = Arc::from(Path::new(path).to_path_buf());
            let fa = Arc::new(FileAnalysis::parse(
                &path,
                text.into(),
                &morog_sema::LintConfig::default(),
                Some(&*analysis.bib),
            ));
            analysis.set_file(file, path, fa);
        }
        let (path, position) = marker.expect("no $0 marker in fixtures");
        let params = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: lsp_types::Url::from_file_path(&path).unwrap(),
            },
            position,
        };
        (
            Snapshot {
                analysis,
                encoding: PositionEncoding::Utf8,
                vault_root: Arc::from(Path::new("/v")),
            },
            params,
        )
    }

    /// Render a `Position` compactly for expect tests.
    pub(crate) fn fmt_position(position: Position) -> String {
        format!("{}:{}", position.line, position.character)
    }

    pub(crate) fn fmt_location(location: &lsp_types::Location) -> String {
        format!(
            "{} {}..{}",
            location.uri.path(),
            fmt_position(location.range.start),
            fmt_position(location.range.end),
        )
    }
}

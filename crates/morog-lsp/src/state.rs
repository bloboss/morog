//! Global server state (plan §5.2): the vfs, the current [`Analysis`]
//! snapshot, configuration, and pending-diagnostics bookkeeping.
//!
//! All writes happen here, on the main thread; request handlers read O(1)
//! clones of the persistent maps on the rayon pool (plan §5.2).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lsp_server::{Message, Response};
use morg_parser::LineIndex;
use morog_bib::BibStore;

use crate::analysis::{Analysis, FileAnalysis};
use crate::config::Config;
use crate::vfs::{FileId, Vfs};
use crate::{PositionEncoding, diagnostics, from_proto, to_proto};

/// A unit of work finished off the main thread.
pub(crate) enum Task {
    /// A computed response, plus the document version its request was
    /// dispatched against (`None` for requests without a primary document).
    Response {
        response: Response,
        stale_guard: Option<(FileId, i32)>,
    },
}

/// The read-only view a request handler computes against: an O(1) clone of
/// the persistent maps (plan §5.2).
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub analysis: Analysis,
    pub encoding: PositionEncoding,
}

pub(crate) struct GlobalState {
    sender: Sender<Message>,
    pub vfs: Vfs,
    pub analysis: Analysis,
    pub config: Config,
    pub encoding: PositionEncoding,
    /// Files whose diagnostics are due at an instant (150 ms debounce).
    pending_diags: HashMap<FileId, Instant>,
    pub task_tx: Sender<Task>,
    pub task_rx: Receiver<Task>,
}

impl GlobalState {
    pub fn new(sender: Sender<Message>, config: Config, encoding: PositionEncoding) -> Self {
        let (task_tx, task_rx) = crossbeam_channel::unbounded();
        Self {
            sender,
            vfs: Vfs::default(),
            analysis: Analysis::default(),
            config,
            encoding,
            pending_diags: HashMap::new(),
            task_tx,
            task_rx,
        }
    }

    /// The bibliography as a lint store; `None` while empty so the
    /// bibliography-backed lints are skipped rather than all-firing.
    fn bib_store(&self) -> Option<&dyn BibStore> {
        (!self.analysis.bib.is_empty()).then_some(&*self.analysis.bib as &dyn BibStore)
    }

    // ---- lifecycle notifications (plan §5.3) ----

    pub fn did_open(&mut self, params: lsp_types::DidOpenTextDocumentParams) {
        let doc = params.text_document;
        let Some(path) = from_proto::abs_path(&doc.uri) else {
            return;
        };
        let file = self.vfs.intern(&path);
        self.vfs.open(file, doc.version, doc.text.into());
        // Opening publishes immediately; only typing debounces.
        self.reanalyze_overlay(file, Instant::now());
    }

    pub fn did_change(&mut self, params: lsp_types::DidChangeTextDocumentParams) {
        let Some(path) = from_proto::abs_path(&params.text_document.uri) else {
            return;
        };
        let Some(file) = self.vfs.get(&path) else {
            return;
        };
        let Some(overlay) = self.vfs.overlay(file) else {
            return;
        };
        let new_text =
            from_proto::apply_changes(&overlay.text, &params.content_changes, self.encoding);
        self.vfs
            .change(file, params.text_document.version, new_text.into());
        self.reanalyze_overlay(file, Instant::now() + diagnostics::DEBOUNCE);
    }

    pub fn did_save(&mut self, params: lsp_types::DidSaveTextDocumentParams) {
        // The embedded indexer reconciles saved files into SQLite
        // (plan §5.3); it arrives with the indexer layer.
        let _ = params;
    }

    pub fn did_close(&mut self, params: lsp_types::DidCloseTextDocumentParams) {
        let Some(path) = from_proto::abs_path(&params.text_document.uri) else {
            return;
        };
        let Some(file) = self.vfs.get(&path) else {
            return;
        };
        self.vfs.close(file);
        self.pending_diags.remove(&file);
        // Clear the editor's diagnostics; the file now belongs to the index.
        self.send_notification::<lsp_types::notification::PublishDiagnostics>(
            lsp_types::PublishDiagnosticsParams {
                uri: params.text_document.uri,
                diagnostics: Vec::new(),
                version: None,
            },
        );
    }

    /// Reparse an open document from its overlay and ripple the xref delta
    /// (plan §5.3 steps 2–6).
    fn reanalyze_overlay(&mut self, file: FileId, deadline: Instant) {
        let Some(overlay) = self.vfs.overlay(file) else {
            return;
        };
        let text = Arc::clone(&overlay.text);
        let path: Arc<Path> = Arc::clone(self.vfs.path(file));
        let fa = Arc::new(FileAnalysis::parse(
            &path,
            text,
            &self.config.lint,
            self.bib_store(),
        ));
        let touched = self.analysis.set_file(file, path, fa);
        self.analysis.generation += 1;
        let mut affected = self.analysis.affected_files(&touched);
        affected.insert(file);
        for file in affected {
            self.schedule_diagnostics(file, deadline);
        }
    }

    // ---- diagnostics debounce (plan §5.3 step 6) ----

    fn schedule_diagnostics(&mut self, file: FileId, deadline: Instant) {
        // Re-scheduling extends the debounce window: a steady typist gets
        // diagnostics once the keystrokes pause.
        let entry = self.pending_diags.entry(file).or_insert(deadline);
        *entry = (*entry).max(deadline);
    }

    pub fn next_diag_deadline(&self) -> Option<Instant> {
        self.pending_diags.values().min().copied()
    }

    /// Wait: time until the next diagnostics deadline, or `None` for "no
    /// pending work, block on the channels".
    pub fn diag_timeout(&self) -> Option<Duration> {
        self.next_diag_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// Publish diagnostics for every file whose debounce ran out.
    pub fn flush_diagnostics(&mut self) {
        let now = Instant::now();
        let due: Vec<FileId> = self
            .pending_diags
            .iter()
            .filter(|&(_, deadline)| *deadline <= now)
            .map(|(&file, _)| file)
            .collect();
        for file in due {
            self.pending_diags.remove(&file);
            self.publish_diagnostics(file);
        }
    }

    /// Push the full `sta::*` set for one open file: file-local lints plus
    /// the vault-level rules recomputed from the xref maps (plan M7 L1).
    fn publish_diagnostics(&mut self, file: FileId) {
        let Some(overlay) = self.vfs.overlay(file) else {
            return; // Closed since scheduling; nothing to publish.
        };
        let version = overlay.version;
        let text = Arc::clone(&overlay.text);
        let Some(uri) = self.analysis.path(file).and_then(|p| to_proto::url(p)) else {
            return;
        };

        let index = LineIndex::new(&text);
        let diagnostics: Vec<lsp_types::Diagnostic> = diagnostics::full_set(&self.analysis, file)
            .iter()
            .map(|d| to_proto::diagnostic(&text, &index, d, self.encoding))
            .collect();

        self.send_notification::<lsp_types::notification::PublishDiagnostics>(
            lsp_types::PublishDiagnosticsParams {
                uri,
                diagnostics,
                version: Some(version),
            },
        );
    }

    // ---- outgoing messages ----

    pub fn send_notification<N: lsp_types::notification::Notification>(&self, params: N::Params) {
        let not = lsp_server::Notification::new(N::METHOD.to_owned(), params);
        let _ = self.sender.send(Message::Notification(not));
    }

    pub fn respond(&self, response: Response) {
        let _ = self.sender.send(Message::Response(response));
    }

    /// Dispatch a request handler onto the rayon pool against a snapshot.
    ///
    /// `uri` names the request's primary document when it has one; its
    /// version at dispatch time becomes the stale guard checked in
    /// [`GlobalState::on_task`] (plan §5.2).
    pub(crate) fn dispatch<F>(&self, id: lsp_server::RequestId, uri: Option<&lsp_types::Url>, f: F)
    where
        F: FnOnce(&Snapshot) -> Result<serde_json::Value, (lsp_server::ErrorCode, String)>
            + Send
            + 'static,
    {
        let stale_guard = uri
            .and_then(from_proto::abs_path)
            .and_then(|path| self.vfs.get(&path))
            .and_then(|file| self.vfs.overlay(file).map(|o| (file, o.version)));
        let snapshot = Snapshot {
            analysis: self.analysis.clone(),
            encoding: self.encoding,
        };
        let task_tx = self.task_tx.clone();
        rayon::spawn(move || {
            let response = match f(&snapshot) {
                Ok(value) => Response::new_ok(id, value),
                Err((code, message)) => Response::new_err(id, code as i32, message),
            };
            let _ = task_tx.send(Task::Response {
                response,
                stale_guard,
            });
        });
    }

    /// Handle one finished task: staleness-check responses before they go
    /// out (plan §5.2 — answer `ContentModified` when the document moved on).
    pub fn on_task(&mut self, task: Task) {
        match task {
            Task::Response {
                response,
                stale_guard,
            } => {
                let stale = stale_guard.is_some_and(|(file, version)| {
                    self.vfs.overlay(file).is_none_or(|o| o.version != version)
                });
                if stale {
                    self.respond(Response::new_err(
                        response.id,
                        lsp_server::ErrorCode::ContentModified as i32,
                        "content modified".to_owned(),
                    ));
                } else {
                    self.respond(response);
                }
            }
        }
    }
}

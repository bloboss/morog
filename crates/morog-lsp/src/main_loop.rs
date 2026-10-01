//! The server's event loop (plan §5.1): one thread multiplexing LSP
//! messages, finished worker tasks, and the diagnostics debounce over a
//! crossbeam `select!`.

use std::time::Instant;

use crossbeam_channel::select;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, Response};
use lsp_types::notification::Notification as _;
use lsp_types::{
    InitializeParams, PositionEncodingKind, SaveOptions, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions,
};

use crate::config::Config;
use crate::handlers;
use crate::state::{GlobalState, Task};
use crate::{Error, PositionEncoding};

/// Run the server over stdio (the `morog lsp` entry point).
pub fn run_stdio() -> Result<(), Error> {
    let (connection, io_threads) = Connection::stdio();
    run(connection)?;
    io_threads.join()?;
    Ok(())
}

/// Drive one connection from `initialize` to `exit`. Used directly by the
/// protocol tests over `Connection::memory()`.
pub fn run(connection: Connection) -> Result<(), Error> {
    let (initialize_id, initialize_params) = connection.initialize_start()?;
    let initialize_params: InitializeParams = serde_json::from_value(initialize_params)?;

    let encoding = negotiate_encoding(&initialize_params);
    let config = Config::from_init(&initialize_params);

    let initialize_result = serde_json::json!({
        "capabilities": server_capabilities(encoding),
        "serverInfo": {
            "name": "morog-lsp",
            "version": env!("CARGO_PKG_VERSION"),
        },
    });
    connection.initialize_finish(initialize_id, initialize_result)?;

    let mut state = GlobalState::new(connection.sender.clone(), config, encoding);
    cold_start(&mut state);
    main_loop(&connection, state)
}

/// Plan §5.4 steps 2–3: build the analysis from a read-only table scan of
/// the index — no reparse — then start the embedded indexer, which
/// reconciles disk against the index and streams deltas.
fn cold_start(state: &mut GlobalState) {
    let vault_root = state
        .config
        .vault_root
        .canonicalize()
        .unwrap_or_else(|_| state.config.vault_root.clone());
    state.config.vault_root = vault_root.clone();

    let db_path = morog_index::cache_db_path(&vault_root);
    if let Ok(conn) = morog_index::schema::open_ro(&db_path) {
        match (
            morog_index::load_all_facts(&conn),
            morog_index::load_bib_entries(&conn),
        ) {
            (Ok(files), Ok(bib)) => {
                state.analysis.bib =
                    std::sync::Arc::new(crate::analysis::BibIndex::from_entries(bib));
                for (rel, facts) in files {
                    state.set_indexed_file(
                        &vault_root.join(rel),
                        crate::analysis::FileAnalysis::from_facts(facts),
                    );
                }
            }
            (files, bib) => {
                let err = files.err().or(bib.err()).expect("one side failed");
                eprintln!("morog-lsp: cold start skipped: {err}");
            }
        }
    }

    state.indexer = Some(crate::indexer::spawn(
        vault_root,
        state.config.bibrat_root.clone(),
        state.task_tx.clone(),
    ));
}

/// Prefer `utf-8` (spans stay byte offsets end to end), fall back to the
/// mandatory `utf-16` (plan §5.4).
fn negotiate_encoding(params: &InitializeParams) -> PositionEncoding {
    let client_encodings = params
        .capabilities
        .general
        .as_ref()
        .and_then(|general| general.position_encodings.as_deref())
        .unwrap_or_default();
    if client_encodings.contains(&PositionEncodingKind::UTF8) {
        PositionEncoding::Utf8
    } else {
        PositionEncoding::Utf16
    }
}

fn server_capabilities(encoding: PositionEncoding) -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(match encoding {
            PositionEncoding::Utf8 => PositionEncodingKind::UTF8,
            PositionEncoding::Utf16 => PositionEncodingKind::UTF16,
        }),
        text_document_sync: Some(TextDocumentSyncCapability::Options(
            TextDocumentSyncOptions {
                open_close: Some(true),
                change: Some(TextDocumentSyncKind::INCREMENTAL),
                save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                })),
                ..Default::default()
            },
        )),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        workspace_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        ..Default::default()
    }
}

/// Dynamically watch the note files (and the bibrat root, when known) —
/// plan §5.4 step 4. Clients without the capability simply error the
/// registration back, which we ignore.
fn register_watchers(state: &mut GlobalState) {
    let mut watchers = vec![serde_json::json!({ "globPattern": "**/*.{md,morg}" })];
    if let Some(bibrat_root) = &state.config.bibrat_root {
        watchers.push(serde_json::json!({
            "globPattern": format!("{}/**", bibrat_root.display()),
        }));
    }
    state.send_request(
        "client/registerCapability",
        serde_json::json!({
            "registrations": [{
                "id": "morog/watchers",
                "method": "workspace/didChangeWatchedFiles",
                "registerOptions": { "watchers": watchers },
            }],
        }),
    );
}

enum Event {
    Lsp(Message),
    Task(Task),
    /// A diagnostics debounce deadline passed.
    Tick,
    /// The client hung up without `exit`.
    Closed,
}

fn main_loop(connection: &Connection, mut state: GlobalState) -> Result<(), Error> {
    loop {
        let event = next_event(connection, &state);
        match event {
            Event::Lsp(Message::Request(request)) => {
                if connection.handle_shutdown(&request)? {
                    return Ok(());
                }
                on_request(&mut state, request);
            }
            Event::Lsp(Message::Notification(notification)) => {
                on_notification(&mut state, notification);
            }
            Event::Lsp(Message::Response(_)) => {}
            Event::Task(task) => state.on_task(task),
            Event::Tick => {}
            Event::Closed => return Ok(()),
        }
        state.flush_diagnostics();
    }
}

fn next_event(connection: &Connection, state: &GlobalState) -> Event {
    let receiver = &connection.receiver;
    let tasks = &state.task_rx;
    match state.diag_timeout() {
        Some(timeout) => {
            let deadline = Instant::now() + timeout;
            select! {
                recv(receiver) -> msg => msg.map_or(Event::Closed, Event::Lsp),
                recv(tasks) -> task => task.map_or(Event::Closed, Event::Task),
                recv(crossbeam_channel::at(deadline)) -> _ => Event::Tick,
            }
        }
        None => {
            select! {
                recv(receiver) -> msg => msg.map_or(Event::Closed, Event::Lsp),
                recv(tasks) -> task => task.map_or(Event::Closed, Event::Task),
            }
        }
    }
}

fn on_request(state: &mut GlobalState, request: Request) {
    use lsp_types::request as r;
    use lsp_types::request::Request as _;

    /// Parse params, extract the stale-guard URI, and dispatch `f` onto the
    /// rayon pool (plan §5.2).
    macro_rules! handle {
        ($request_type:ty, $uri:expr, $f:expr) => {{
            match serde_json::from_value::<<$request_type as r::Request>::Params>(request.params) {
                Ok(params) => {
                    #[allow(clippy::redundant_closure_call)]
                    let uri: Option<lsp_types::Url> = ($uri)(&params);
                    state.dispatch(request.id, uri.as_ref(), move |snapshot| {
                        $f(snapshot, params).and_then(|result| {
                            serde_json::to_value(result)
                                .map_err(|e| (ErrorCode::InternalError, e.to_string()))
                        })
                    });
                }
                Err(e) => state.respond(Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    e.to_string(),
                )),
            }
        }};
    }

    match request.method.as_str() {
        r::HoverRequest::METHOD => handle!(
            r::HoverRequest,
            |p: &lsp_types::HoverParams| Some(
                p.text_document_position_params.text_document.uri.clone()
            ),
            handlers::hover::handle
        ),
        r::GotoDefinition::METHOD => handle!(
            r::GotoDefinition,
            |p: &lsp_types::GotoDefinitionParams| Some(
                p.text_document_position_params.text_document.uri.clone()
            ),
            handlers::goto_definition::handle
        ),
        r::References::METHOD => handle!(
            r::References,
            |p: &lsp_types::ReferenceParams| Some(
                p.text_document_position.text_document.uri.clone()
            ),
            handlers::references::handle
        ),
        r::DocumentSymbolRequest::METHOD => handle!(
            r::DocumentSymbolRequest,
            |p: &lsp_types::DocumentSymbolParams| Some(p.text_document.uri.clone()),
            handlers::document_symbol::handle
        ),
        r::WorkspaceSymbolRequest::METHOD => handle!(
            r::WorkspaceSymbolRequest,
            |_: &lsp_types::WorkspaceSymbolParams| None::<lsp_types::Url>,
            handlers::workspace_symbol::handle
        ),
        _ => {
            // Everything else is politely refused — through the worker
            // path, so the response pipeline has a single shape.
            let method = request.method;
            state.dispatch(request.id, None, move |_snapshot| {
                Err((
                    ErrorCode::MethodNotFound,
                    format!("morog-lsp does not handle `{method}` yet"),
                ))
            });
        }
    }
}

fn on_notification(state: &mut GlobalState, notification: Notification) {
    use lsp_types::notification as n;
    match notification.method.as_str() {
        n::Initialized::METHOD => register_watchers(state),
        n::DidChangeWatchedFiles::METHOD => {
            if let Ok(params) = serde_json::from_value(notification.params) {
                state.did_change_watched_files(params);
            }
        }
        n::DidOpenTextDocument::METHOD => {
            if let Ok(params) = serde_json::from_value(notification.params) {
                state.did_open(params);
            }
        }
        n::DidChangeTextDocument::METHOD => {
            if let Ok(params) = serde_json::from_value(notification.params) {
                state.did_change(params);
            }
        }
        n::DidSaveTextDocument::METHOD => {
            if let Ok(params) = serde_json::from_value(notification.params) {
                state.did_save(params);
            }
        }
        n::DidCloseTextDocument::METHOD => {
            if let Ok(params) = serde_json::from_value(notification.params) {
                state.did_close(params);
            }
        }
        _ => {}
    }
}

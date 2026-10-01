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

    // The resolved `[tags]` table (plan §10 T1): vault-local `.morog.toml`
    // over morog's config over morg's config — the same resolution as the
    // CLI (`morog_sema::tags` is the shared implementation). The same three
    // files become the live-reconfiguration watch list.
    let (tag_table, warnings) = morog_sema::tags::load_tag_table(&vault_root);
    for warning in warnings {
        eprintln!("morog-lsp: config: {warning}");
    }
    state.analysis.tag_table = std::sync::Arc::new(tag_table);
    state.tag_config_paths = morog_sema::tags::tag_config_paths(&vault_root);

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

    // Cold tags (plan §4.1 B6): the `TAGS.yml` vocabulary is not persisted
    // in SQLite, so read it straight off the store root — tag completion
    // works from the first keystroke instead of waiting for the first
    // indexer reconcile to stream it.
    if let Some(watch) = state.config.bib_watch() {
        state.analysis.bib_tags = morog_bib::load_root_tags(&watch.root).into();
    }

    state.indexer = Some(crate::indexer::spawn(
        vault_root,
        state.config.bibrat_root.clone(),
        std::sync::Arc::clone(&state.analysis.tag_table),
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
        completion_provider: Some(lsp_types::CompletionOptions {
            // `[@` citations, `id:` links, `#` tags, `key: ` values.
            trigger_characters: Some(vec!["@".into(), ":".into(), "#".into(), "[".into()]),
            ..Default::default()
        }),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        inlay_hint_provider: Some(lsp_types::OneOf::Left(true)),
        semantic_tokens_provider: Some(
            lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                lsp_types::SemanticTokensOptions {
                    legend: handlers::semantic_tokens::legend(),
                    full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                    ..Default::default()
                },
            ),
        ),
        rename_provider: Some(lsp_types::OneOf::Right(lsp_types::RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: Default::default(),
        })),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
        execute_command_provider: Some(lsp_types::ExecuteCommandOptions {
            commands: vec![RESEARCH_NEW_COMMAND.to_owned()],
            ..Default::default()
        }),
        // The custom request of plan §5.6, advertised for feature probing.
        experimental: Some(serde_json::json!({ "morog/neighbourhood": true })),
        definition_provider: Some(lsp_types::OneOf::Left(true)),
        references_provider: Some(lsp_types::OneOf::Left(true)),
        document_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        workspace_symbol_provider: Some(lsp_types::OneOf::Left(true)),
        ..Default::default()
    }
}

/// Dynamically watch the note files, the bibrat store, and the `[tags]`
/// config files — plan §5.4 step 4, §10 T1. The bibrat data directory (and
/// its `config.toml`), like the XDG config files, usually lives outside the
/// workspace folder, so those watchers use a `RelativePattern` with an
/// absolute `baseUri` when the client supports it, else an absolute-path
/// glob string — the best a plain pattern can do.
///
/// Fallback: clients without dynamic watchers error the registration back
/// (ignored), and clients that silently refuse to watch outside the
/// workspace never send bibrat events. Either way the bibliography and tag
/// vocabulary still load at cold start and refresh on every reconcile;
/// they just stop reacting to external bibrat edits live. No periodic
/// refresh is attempted by design.
fn register_watchers(state: &mut GlobalState) {
    let mut watchers = vec![serde_json::json!({ "globPattern": "**/*.{md,morg}" })];
    {
        let relative = state.config.relative_pattern_support;
        let mut push = |base: &std::path::Path, pattern: &str| {
            let glob = relative
                .then(|| lsp_types::Url::from_file_path(base).ok())
                .flatten()
                .map(|uri| serde_json::json!({ "baseUri": uri, "pattern": pattern }))
                .unwrap_or_else(|| serde_json::json!(format!("{}/{pattern}", base.display())));
            watchers.push(serde_json::json!({ "globPattern": glob }));
        };
        if let Some(watch) = state.config.bib_watch() {
            push(&watch.root, "**");
            if let Some(config) = &watch.config
                && let (Some(dir), Some(name)) = (config.parent(), config.file_name())
            {
                push(dir, &name.to_string_lossy());
            }
        }
        // The three `[tags]` config files (plan §10 T1): the vault-local
        // `.morog.toml` is a dotfile the note glob misses, and the two XDG
        // files live outside the workspace, so each gets its own watcher —
        // the bibrat-config approach.
        for config in &state.tag_config_paths {
            if let (Some(dir), Some(name)) = (config.parent(), config.file_name()) {
                push(dir, &name.to_string_lossy());
            }
        }
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
        r::Completion::METHOD => handle!(
            r::Completion,
            |p: &lsp_types::CompletionParams| Some(
                p.text_document_position.text_document.uri.clone()
            ),
            handlers::completion::handle
        ),
        r::CodeActionRequest::METHOD => handle!(
            r::CodeActionRequest,
            |p: &lsp_types::CodeActionParams| Some(p.text_document.uri.clone()),
            handlers::code_action::handle
        ),
        // Custom morog/neighbourhood (plan §5.6): the subgraph around a
        // note, from the xref — same worker path as the standard requests.
        "morog/neighbourhood" => {
            match serde_json::from_value::<handlers::neighbourhood::NeighbourhoodParams>(
                request.params,
            ) {
                Ok(params) => state.dispatch(request.id, None, move |snapshot| {
                    handlers::neighbourhood::handle(snapshot, params)
                }),
                Err(e) => state.respond(Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    e.to_string(),
                )),
            }
        }
        // workspace/executeCommand mutates the vault, so it runs right here
        // on the main thread (a single file write) rather than on a worker.
        r::ExecuteCommand::METHOD => {
            match serde_json::from_value::<lsp_types::ExecuteCommandParams>(request.params) {
                Ok(params) => execute_command(state, request.id, params),
                Err(e) => state.respond(Response::new_err(
                    request.id,
                    ErrorCode::InvalidParams as i32,
                    e.to_string(),
                )),
            }
        }
        r::PrepareRenameRequest::METHOD => handle!(
            r::PrepareRenameRequest,
            |p: &lsp_types::TextDocumentPositionParams| Some(p.text_document.uri.clone()),
            handlers::rename::prepare
        ),
        r::Rename::METHOD => handle!(
            r::Rename,
            |p: &lsp_types::RenameParams| Some(p.text_document_position.text_document.uri.clone()),
            handlers::rename::handle
        ),
        r::InlayHintRequest::METHOD => handle!(
            r::InlayHintRequest,
            |p: &lsp_types::InlayHintParams| Some(p.text_document.uri.clone()),
            handlers::inlay_hint::handle
        ),
        r::SemanticTokensFullRequest::METHOD => handle!(
            r::SemanticTokensFullRequest,
            |p: &lsp_types::SemanticTokensParams| Some(p.text_document.uri.clone()),
            handlers::semantic_tokens::handle
        ),
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

/// The one command of plan §5.6: `morog.research.new`.
pub(crate) const RESEARCH_NEW_COMMAND: &str = "morog.research.new";

/// `workspace/executeCommand`: create a note and answer its URI.
fn execute_command(
    state: &mut GlobalState,
    id: lsp_server::RequestId,
    params: lsp_types::ExecuteCommandParams,
) {
    if params.command != RESEARCH_NEW_COMMAND {
        state.respond(Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            format!("unknown command `{}`", params.command),
        ));
        return;
    }
    match research_new(state, &params.arguments) {
        Ok(value) => state.respond(Response::new_ok(id, value)),
        Err(message) => state.respond(Response::new_err(
            id,
            ErrorCode::InvalidParams as i32,
            message,
        )),
    }
}

/// `morog.research.new {kind, title}`: the `morog research` creation path
/// (kind and title; for a source the title **is** the citation key), writing
/// `<vault>/<id>.md` and returning `{ "uri": … }`.
fn research_new(
    state: &mut GlobalState,
    arguments: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    #[derive(serde::Deserialize)]
    struct Args {
        kind: String,
        title: String,
    }
    let args: Args = match arguments {
        [object] => serde_json::from_value(object.clone()).map_err(|e| e.to_string())?,
        [kind, title] => Args {
            kind: kind.as_str().unwrap_or_default().to_owned(),
            title: title.as_str().unwrap_or_default().to_owned(),
        },
        _ => return Err("expected arguments: {\"kind\": …, \"title\": …}".to_owned()),
    };
    let kind = morog_sema::NoteKind::parse(&args.kind)
        .ok_or_else(|| format!("unknown kind `{}`", args.kind))?;

    let (id, content) = match kind {
        morog_sema::NoteKind::Source => {
            let key = args.title.as_str();
            if !morog_sema::is_slug(key) {
                return Err(format!("`{key}` is not a slug-shaped citation key"));
            }
            let bib = &state.analysis.bib;
            let entry = bib.get(key);
            if entry.is_none() && !bib.is_empty() {
                return Err(format!(
                    "citation key `{key}` not found in the bibliography"
                ));
            }
            let title = entry
                .and_then(|e| e.meta.as_ref())
                .and_then(|m| m.title.clone());
            (
                key.to_owned(),
                crate::research::source_note(key, title.as_deref()),
            )
        }
        morog_sema::NoteKind::Topic => {
            let id = crate::research::slugify(&args.title)
                .ok_or_else(|| format!("cannot derive a slug id from `{}`", args.title))?;
            let content = crate::research::topic_note(&id, &args.title);
            (id, content)
        }
        morog_sema::NoteKind::Argument => {
            let id = crate::research::slugify(&args.title)
                .ok_or_else(|| format!("cannot derive a slug id from `{}`", args.title))?;
            let content = crate::research::argument_note(&id, &args.title);
            (id, content)
        }
    };

    if state.analysis.xref.by_id.contains_key(id.as_str()) {
        return Err(format!("id `{id}` is already declared in the vault"));
    }
    let path = state.config.vault_root.join(format!("{id}.md"));
    let uri = lsp_types::Url::from_file_path(&path)
        .map_err(|()| format!("vault path `{}` is not absolute", path.display()))?;

    // Like `morog research`: never overwrite.
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    open.open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, content.as_bytes()))
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;

    // Make the note visible to requests immediately; the indexer writes
    // SQLite in the background (it remains the sole writer).
    let tags = std::sync::Arc::clone(&state.analysis.tag_table);
    let fa = crate::analysis::FileAnalysis::parse(
        &path,
        content.into(),
        &state.config.lint,
        state.bib_store(),
        &tags,
    );
    state.set_indexed_file(&path, fa);
    if let Some(indexer) = &state.indexer {
        indexer.notify_changed(path);
    }
    Ok(serde_json::json!({ "uri": uri }))
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

//! The server's event loop (plan §5.1): one thread multiplexing LSP
//! messages, finished worker tasks, and the diagnostics debounce over a
//! crossbeam `select!`.

use std::time::Instant;

use crossbeam_channel::select;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request};
use lsp_types::notification::Notification as _;
use lsp_types::{
    InitializeParams, PositionEncodingKind, SaveOptions, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions,
};

use crate::config::Config;
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

    let state = GlobalState::new(connection.sender.clone(), config, encoding);
    main_loop(&connection, state)
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
        ..Default::default()
    }
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
    // The navigation handlers (hover, definition, references, symbols)
    // arrive with L2; everything else is politely refused — through the
    // worker path, so the response pipeline has a single shape.
    let method = request.method;
    state.dispatch(request.id, None, move |_snapshot| {
        Err((
            ErrorCode::MethodNotFound,
            format!("morog-lsp does not handle `{method}` yet"),
        ))
    });
}

fn on_notification(state: &mut GlobalState, notification: Notification) {
    use lsp_types::notification as n;
    match notification.method.as_str() {
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

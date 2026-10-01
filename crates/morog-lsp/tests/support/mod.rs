//! Shared protocol-test client: drives a real server over
//! `Connection::memory()` (plan §5.7).
//!
//! Compiled into several test binaries, each using a different subset.
#![allow(dead_code)]

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use serde_json::{Value, json};

pub const TIMEOUT: Duration = Duration::from_secs(30);

/// A test client driving a real server on a background thread.
pub struct Client {
    pub connection: Connection,
    server: Option<std::thread::JoinHandle<Result<(), morog_lsp::Error>>>,
    pub next_id: i32,
}

impl Client {
    /// Start a server and run `initialize` with the given client encodings.
    pub fn start(encodings: &[&str], init_options: Value) -> Self {
        let (server_side, client_side) = Connection::memory();
        let server = std::thread::spawn(move || morog_lsp::run(server_side));
        let mut client = Self {
            connection: client_side,
            server: Some(server),
            next_id: 0,
        };
        let response = client.request(
            "initialize",
            json!({
                "capabilities": { "general": { "positionEncodings": encodings } },
                "initializationOptions": init_options,
            }),
        );
        let encoding = &response["capabilities"]["positionEncoding"];
        let expected = if encodings.contains(&"utf-8") {
            "utf-8"
        } else {
            "utf-16"
        };
        assert_eq!(encoding, expected, "negotiated encoding");
        client.notify("initialized", json!({}));
        client
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = RequestId::from(self.next_id);
        self.connection
            .sender
            .send(Message::Request(Request::new(
                id.clone(),
                method.to_owned(),
                params,
            )))
            .unwrap();
        loop {
            match self.recv() {
                Message::Response(response) => {
                    assert_eq!(response.id, id);
                    return response
                        .response_result
                        .unwrap_or_else(|e| panic!("request `{method}` failed: {e:?}"));
                }
                // Skip server-initiated notifications and requests
                // (diagnostics, progress, registrations).
                Message::Notification(_) => continue,
                Message::Request(_) => continue,
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        self.connection
            .sender
            .send(Message::Notification(Notification::new(
                method.to_owned(),
                params,
            )))
            .unwrap();
    }

    pub fn recv(&self) -> Message {
        self.connection
            .receiver
            .recv_timeout(TIMEOUT)
            .expect("server went quiet")
    }

    /// Wait for the next publishDiagnostics for `uri`.
    pub fn recv_diagnostics(&self, uri: &str) -> Value {
        loop {
            if let Message::Notification(notification) = self.recv()
                && notification.method == "textDocument/publishDiagnostics"
                && notification.params["uri"] == uri
            {
                return notification.params;
            }
        }
    }

    pub fn open(&self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri, "languageId": "markdown",
                    "version": version, "text": text,
                }
            }),
        );
    }

    pub fn shutdown(mut self) {
        let _ = self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        self.server
            .take()
            .unwrap()
            .join()
            .expect("server thread panicked")
            .expect("server returned an error");
    }
}

/// The diagnostic codes of one publishDiagnostics params value.
pub fn codes(diags: &Value) -> Vec<String> {
    diags["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap_or_default().to_owned())
        .collect()
}

//! The server over a real LSP connection: the handshake, live diagnostics as the
//! document changes, and each request answered in the protocol's own shapes.

use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::*;
use serde_json::{json, Value};

/// A client talking to the server on a background thread.
struct Client {
    conn: Connection,
    handle: Option<std::thread::JoinHandle<()>>,
    next_id: i32,
    opened: bool,
    capabilities: Option<Value>,
}

impl Client {
    fn start() -> Client {
        let (server, client) = Connection::memory();
        let handle = std::thread::spawn(move || {
            let (packages, _) = meadow::stdlib::std_packages(meadow::Options::debug());
            meadow_lsp::server::serve(&server, packages).expect("server");
        });

        let mut c = Client {
            conn: client,
            handle: Some(handle),
            next_id: 0,
            opened: false,
            capabilities: None,
        };
        // `Connection::initialize` waits for the request, then for `initialized`.
        let caps = c.request("initialize", json!({"capabilities": {}}));
        c.notify("initialized", json!({}));
        c.capabilities = Some(caps);
        c
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.conn
            .sender
            .send(Message::Request(Request {
                id: id.into(),
                method: method.into(),
                params,
            }))
            .unwrap();
        loop {
            match self.conn.receiver.recv().unwrap() {
                Message::Response(Response {
                    response_result, ..
                }) => match response_result {
                    Ok(v) => return v,
                    Err(e) => panic!("{method} failed: {e:?}"),
                },
                // Diagnostics can arrive between a request and its response.
                Message::Notification(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        self.conn
            .sender
            .send(Message::Notification(Notification {
                method: method.into(),
                params,
            }))
            .unwrap();
    }

    /// Open or replace a document and return the diagnostics it produced.
    fn set(&mut self, text: &str) -> Vec<Diagnostic> {
        if self.opened {
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {"uri": URI, "version": 2},
                    "contentChanges": [{"text": text}]
                }),
            );
        } else {
            self.opened = true;
            self.notify(
                "textDocument/didOpen",
                json!({"textDocument": {
                    "uri": URI, "languageId": "meadow", "version": 1, "text": text
                }}),
            );
        }
        loop {
            if let Message::Notification(n) = self.conn.receiver.recv().unwrap() {
                if n.method == "textDocument/publishDiagnostics" {
                    let p: PublishDiagnosticsParams = serde_json::from_value(n.params).unwrap();
                    return p.diagnostics;
                }
            }
        }
    }

    fn at(&mut self, method: &str, line: u32, character: u32) -> Value {
        self.request(
            method,
            json!({
                "textDocument": {"uri": URI},
                "position": {"line": line, "character": character}
            }),
        )
    }
}

const URI: &str = "file:///main.mw";

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        drop(std::mem::replace(&mut self.conn, Connection::memory().0));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

const SRC: &str = "\
-- Doubles its argument.
fun double n = n * 2

def main = double 21
";

#[test]
fn the_handshake_advertises_what_we_implement() {
    let c = Client::start();
    let caps = c.capabilities.as_ref().unwrap();
    let s = &caps["capabilities"];
    assert_eq!(s["hoverProvider"], json!(true));
    assert_eq!(s["definitionProvider"], json!(true));
    assert_eq!(s["inlayHintProvider"], json!(true));
    assert!(s["semanticTokensProvider"].is_object());
    assert_eq!(s["textDocumentSync"], json!(1), "full sync");
}

#[test]
fn diagnostics_arrive_on_open_and_update_on_every_edit() {
    let mut c = Client::start();

    assert!(c.set(SRC).is_empty(), "a good file reports nothing");

    // Break it: diagnostics must appear without any request being made.
    let broken = c.set("def main = 1 + \"oops\"\n");
    assert_eq!(broken.len(), 1);
    assert!(broken[0].message.contains("type mismatch"), "{:?}", broken[0]);
    assert_eq!(broken[0].severity, Some(DiagnosticSeverity::ERROR));
    assert_eq!(broken[0].range.start.line, 0);

    // …and disappear again when it is fixed.
    assert!(c.set(SRC).is_empty(), "the error should clear");
}

#[test]
fn a_parse_error_is_reported_rather_than_crashing_the_server() {
    let mut c = Client::start();
    let diags = c.set("def main = let in\n");
    assert!(!diags.is_empty());
    // The server is still answering afterwards.
    assert!(c.set(SRC).is_empty());
}

#[test]
fn hover_shows_the_signature_and_the_doc_comment() {
    let mut c = Client::start();
    c.set(SRC);
    let hover = c.at("textDocument/hover", 3, 12);
    let text = hover["contents"]["value"].as_str().unwrap();
    assert!(text.contains("double : Int -> Int"), "{text}");
    assert!(text.contains("Doubles its argument."), "{text}");
}

#[test]
fn hover_on_empty_space_is_null_not_an_error() {
    let mut c = Client::start();
    c.set(SRC);
    assert_eq!(c.at("textDocument/hover", 2, 0), Value::Null);
}

#[test]
fn go_to_definition_points_at_the_binder() {
    let mut c = Client::start();
    c.set(SRC);
    let def = c.at("textDocument/definition", 3, 12);
    assert_eq!(def["uri"], json!(URI));
    assert_eq!(def["range"]["start"], json!({"line": 1, "character": 4}));
    assert_eq!(def["range"]["end"], json!({"line": 1, "character": 10}));
}

#[test]
fn inlay_hints_annotate_parameters() {
    let mut c = Client::start();
    c.set(SRC);
    let hints = c.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": {"uri": URI},
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 9, "character": 0}}
        }),
    );
    let hints = hints.as_array().unwrap();
    assert!(!hints.is_empty(), "expected a hint for `n`");
    assert_eq!(hints[0]["label"], json!(" : Int"));
    // Just after `n` in `fun double n = n * 2`.
    assert_eq!(hints[0]["position"], json!({"line": 1, "character": 12}));
}

#[test]
fn semantic_tokens_come_back_as_deltas() {
    let mut c = Client::start();
    c.set(SRC);
    let st = c.request(
        "textDocument/semanticTokens/full",
        json!({"textDocument": {"uri": URI}}),
    );
    let data = st["data"].as_array().unwrap();
    assert!(!data.is_empty());
    assert_eq!(data.len() % 5, 0, "five numbers per token");
    // The first token is `fun` on line 1 (line 0 is a comment, which the lexer
    // discards), so the first delta-line is 1 and the type is `keyword` (0).
    assert_eq!(data[0], json!(1));
    assert_eq!(data[3], json!(0));
}

#[test]
fn an_unknown_request_is_an_error_not_a_panic() {
    let mut c = Client::start();
    c.set(SRC);
    c.next_id += 1;
    let id = c.next_id;
    c.conn
        .sender
        .send(Message::Request(Request {
            id: id.into(),
            method: "textDocument/nonsense".into(),
            params: json!({}),
        }))
        .unwrap();
    loop {
        if let Message::Response(r) = c.conn.receiver.recv().unwrap() {
            assert!(
                r.response_result.is_err(),
                "should report method not found"
            );
            break;
        }
    }
}

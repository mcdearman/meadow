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
        Client::start_with(None)
    }

    /// The same, with the standard library's sources written out somewhere --
    /// which is what lets a definition inside `Std` be named as a file.
    fn start_with(std_src_root: Option<std::path::PathBuf>) -> Client {
        let (server, client) = Connection::memory();
        let handle = std::thread::spawn(move || {
            let opts = meadow::Options::debug();
        let (packages, _) = meadow::stdlib::std_packages(opts);
        let modules = meadow::stdlib::std_modules(opts)
            .0
            .into_iter()
            .map(|(dotted, pkg)| (dotted.to_string(), pkg))
            .collect();
            meadow_lsp::server::serve(&server, packages, modules, std_src_root, None).expect("server");
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

    // The label is a *sequence* of parts, not a string: a type name in it has
    // to be able to carry a `location`, which is what an editor turns into a
    // ctrl-click. Flattened, the hints read as the annotation you could have
    // written — `fun double (n : Int) : Int = n * 2`.
    let flat: Vec<(u64, u64, String)> = hints
        .iter()
        .map(|h| {
            let parts = h["label"].as_array().expect("label parts");
            let text: String = parts
                .iter()
                .map(|p| p["value"].as_str().unwrap_or_default())
                .collect();
            (
                h["position"]["line"].as_u64().unwrap(),
                h["position"]["character"].as_u64().unwrap(),
                text,
            )
        })
        .collect();

    // `fun double n = n * 2` — an open paren before `n`, and the rest after it.
    assert!(flat.contains(&(1, 11, "(".to_string())), "got {flat:?}");
    assert!(
        flat.contains(&(1, 12, " : Int) : Int".to_string())),
        "the closing paren and the result type share a position: {flat:?}"
    );
}

/// A type name inside a hint carries a `location`, which is what makes it
/// ctrl-clickable — and a builtin, which is declared nowhere, does not.
#[test]
fn a_type_name_in_a_hint_is_a_link() {
    let root = std_sources("hint-link");
    let mut c = Client::start_with(Some(root.clone()));
    c.set("fun pick m = match m with | Just v -> v | None -> 0\n");
    let hints = c.request(
        "textDocument/inlayHint",
        json!({
            "textDocument": {"uri": URI},
            "range": {"start": {"line": 0, "character": 0},
                      "end": {"line": 9, "character": 0}}
        }),
    );

    let mut linked = Vec::new();
    let mut plain = Vec::new();
    for h in hints.as_array().unwrap() {
        for p in h["label"].as_array().unwrap() {
            let v = p["value"].as_str().unwrap_or_default().to_string();
            if p.get("location").is_some() {
                linked.push(v);
            } else {
                plain.push(v);
            }
        }
    }
    assert!(linked.contains(&"Maybe".to_string()), "linked: {linked:?}");
    assert!(plain.contains(&"Int".to_string()), "plain: {plain:?}");

    drop(c);
    let _ = std::fs::remove_dir_all(&root);
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

// --- the handshake ---------------------------------------------------------

/// Drive the handshake by hand, sending `chatter` between the `initialize`
/// response and `initialized`, and answer whether the server survived.
fn survives_chatter_before_initialized(chatter: &[(&str, Value)]) -> bool {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || {
        let opts = meadow::Options::debug();
        let (packages, _) = meadow::stdlib::std_packages(opts);
        let modules = meadow::stdlib::std_modules(opts)
            .0
            .into_iter()
            .map(|(dotted, pkg)| (dotted.to_string(), pkg))
            .collect();
        let _ = meadow_lsp::server::serve(&server, packages, modules, None, None);
    });

    client
        .sender
        .send(Message::Request(Request {
            id: 1.into(),
            method: "initialize".into(),
            params: json!({"capabilities": {}}),
        }))
        .unwrap();
    // Wait for the initialize response before the chatter, which is the window
    // the strict handshake used to die in.
    loop {
        match client.receiver.recv().unwrap() {
            Message::Response(_) => break,
            _ => continue,
        }
    }
    for (method, params) in chatter {
        client
            .sender
            .send(Message::Notification(Notification {
                method: (*method).into(),
                params: params.clone(),
            }))
            .unwrap();
    }
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".into(),
            params: json!({}),
        }))
        .unwrap();

    // If the server is alive it will answer this; if it died, the channel
    // closes and `recv` fails.
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".into(),
            params: json!({"textDocument": {
                "uri": "file:///x.mw", "languageId": "meadow", "version": 1,
                "text": "def main = 1\n"
            }}),
        }))
        .unwrap();
    let alive = loop {
        match client.receiver.recv() {
            Ok(Message::Notification(n)) if n.method == "textDocument/publishDiagnostics" => {
                break true;
            }
            Ok(_) => continue,
            Err(_) => break false,
        }
    };

    drop(client);
    let _ = handle.join();
    alive
}

#[test]
fn the_handshake_tolerates_what_vs_code_actually_sends() {
    // `lsp_server::Connection::initialize` insists `initialized` is the very
    // next message and treats anything else as fatal. VS Code sends `$/setTrace`
    // and `workspace/didChangeConfiguration` around it, and the server used to
    // exit during startup — which reached the user as `write EPIPE, shutting
    // down server`, a message about the client's failed write rather than about
    // anything the server did.
    assert!(
        survives_chatter_before_initialized(&[("$/setTrace", json!({"value": "off"}))]),
        "$/setTrace before `initialized` killed the server"
    );
    assert!(
        survives_chatter_before_initialized(&[(
            "workspace/didChangeConfiguration",
            json!({"settings": {}})
        )]),
        "didChangeConfiguration before `initialized` killed the server"
    );
    assert!(
        survives_chatter_before_initialized(&[]),
        "the ordinary handshake should still work"
    );
}

#[test]
fn a_request_before_initialized_is_answered_rather_than_dropped() {
    // Otherwise the client waits on it forever.
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || {
        let opts = meadow::Options::debug();
        let (packages, _) = meadow::stdlib::std_packages(opts);
        let modules = meadow::stdlib::std_modules(opts)
            .0
            .into_iter()
            .map(|(dotted, pkg)| (dotted.to_string(), pkg))
            .collect();
        let _ = meadow_lsp::server::serve(&server, packages, modules, None, None);
    });
    client
        .sender
        .send(Message::Request(Request {
            id: 1.into(),
            method: "initialize".into(),
            params: json!({"capabilities": {}}),
        }))
        .unwrap();
    loop {
        if let Message::Response(_) = client.receiver.recv().unwrap() {
            break;
        }
    }
    client
        .sender
        .send(Message::Request(Request {
            id: 2.into(),
            method: "textDocument/hover".into(),
            params: json!({}),
        }))
        .unwrap();
    let answered = match client.receiver.recv() {
        Ok(Message::Response(r)) => r.response_result.is_err(),
        _ => false,
    };
    drop(client);
    let _ = handle.join();
    assert!(answered, "an early request should get an error, not silence");
}

/// Go-to-definition answers with *another file's* URI.
///
/// The handler used to echo back the URI it was asked about, so the only way to
/// tell a working cross-module jump from a broken one is over the protocol,
/// where the `uri` field is visible. Everything below the protocol could be
/// right and this still wrong.
#[test]
fn definition_crosses_into_the_standard_library() {
    let root = std_sources("definition");
    let mut c = Client::start_with(Some(root.clone()));
    c.set("use Std.String (concatAll)\ndef main = concatAll\n");
    // Line 1, character 12 is inside `concatAll` in `def main = concatAll`.
    let def = c.at("textDocument/definition", 1, 12);

    let uri = def["uri"].as_str().expect("a uri");
    assert_ne!(uri, URI, "the definition is not in the open document");
    assert!(
        uri.starts_with("file:///"),
        "expected a file URI, got {uri}"
    );
    assert!(
        uri.ends_with("/Std/String.mw"),
        "expected Std/String.mw, got {uri}"
    );
    // And it points at a real line in that file, not at 0:0.
    assert!(
        def["range"]["start"]["line"].as_u64().unwrap() > 0,
        "got {:?}",
        def["range"]
    );

    drop(c);
    let _ = std::fs::remove_dir_all(&root);
}

/// The standard library's sources, written somewhere an editor could open them.
///
/// Deliberately not `stdlib::extract_sources`: that writes to the user's home,
/// and what is under test here is the server, not where the files are kept.
/// `who` keeps concurrent tests out of each other's directory.
fn std_sources(who: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("meadow-lsp-std-{}-{who}", std::process::id()));
    for (dotted, src) in meadow::stdlib::MODULES {
        let path = root.join(format!("Std/{}.mw", dotted.replace('.', "/")));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, src).unwrap();
    }
    root
}

#[test]
fn rename_is_offered_and_answers_with_an_edit_per_occurrence() {
    let mut c = Client::start();
    let caps = c.capabilities.clone().unwrap();
    assert!(
        caps["capabilities"]["renameProvider"]["prepareProvider"] == json!(true),
        "rename is not advertised: {caps}"
    );
    c.set(SRC);

    // The box the editor opens is over the identifier itself.
    let prepared = c.at("textDocument/prepareRename", 1, 5);
    assert_eq!(prepared["start"], json!({"line": 1, "character": 4}));
    assert_eq!(prepared["end"], json!({"line": 1, "character": 10}));

    let edit = c.request(
        "textDocument/rename",
        json!({
            "textDocument": {"uri": URI},
            "position": {"line": 1, "character": 5},
            "newName": "twice"
        }),
    );
    let edits = edit["changes"][URI].as_array().expect("edits for this file");
    // The declaration and the one call.
    assert_eq!(edits.len(), 2, "{edit}");
    for e in edits {
        assert_eq!(e["newText"], json!("twice"));
    }
}

#[test]
fn rename_refuses_a_name_from_the_standard_library() {
    let mut c = Client::start();
    c.set("def main = println \"hi\"\n");
    // `println` is `Std`'s, and this server is not editing `Std`.
    let prepared = c.at("textDocument/prepareRename", 0, 12);
    assert_eq!(prepared, Value::Null, "offered a rename it cannot do");
}

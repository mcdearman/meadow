//! The protocol loop: `meadow lsp` speaks LSP over stdin/stdout.
//!
//! Synchronous and single-threaded on purpose. A request is answered by
//! re-analysing the document it names, and analysis is fast enough that a queue
//! and a cancellation protocol would be more machinery than the problem needs.
//! `Std` is compiled once at startup, which is the only slow part.

use crate::analysis::{Analysis, HintPart, Loc, Std};
use crate::pos::LineIndex;
use crate::tokens;
use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use meadow_compiler::{source::Source, span::Span, CompiledPackage};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized,
    Notification, PublishDiagnostics,
};
use lsp_types::request::{
    GotoDefinition, HoverRequest, InlayHintRequest, Request as LspRequest,
    SemanticTokensFullRequest,
};
use lsp_types::*;
use std::collections::HashMap;
use std::error::Error;

pub fn run(
    std_packages: Vec<CompiledPackage>,
    std_modules: Vec<(String, CompiledPackage)>,
    std_src_root: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    serve(&connection, std_packages, std_modules, std_src_root)?;
    // The writer thread runs until its channel disconnects, which only happens
    // when the last `Connection` is gone. Joining while this one is still in
    // scope hangs the process — an editor would leave a stray server behind on
    // every close.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Handshake, then answer requests until the client says to stop.
///
/// Separate from [`run`] so a test can drive it over `Connection::memory()`
/// rather than a real pair of pipes.
pub fn serve(
    connection: &Connection,
    std_packages: Vec<CompiledPackage>,
    std_modules: Vec<(String, CompiledPackage)>,
    // Where the embedded `Std` sources were written out, if anywhere — what
    // makes a definition inside the library a file an editor can open.
    std_src_root: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    handshake(connection)?;
    let mut server = Server {
        std: Std::new(std_packages, std_modules, std_src_root),
        docs: HashMap::new(),
        indexes: HashMap::new(),
    };
    server.main_loop(connection)
}

/// Answer `initialize`, then wait for `initialized`.
///
/// `lsp_server::Connection::initialize` does this too, but insists that
/// `initialized` be the *very next* message and treats anything else as fatal.
/// VS Code does not always oblige: `$/setTrace` and
/// `workspace/didChangeConfiguration` can arrive first, and then the server
/// exits during startup. What the user sees is not that — it is
/// `write EPIPE, shutting down server` from a client writing to a process that
/// is already gone, which says nothing about why.
///
/// So: wait for `initialized`, and let anything else past. A request that
/// arrives this early is answered with `ServerNotInitialized` rather than
/// dropped, so the client is not left waiting on it forever.
fn handshake(connection: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
    let (id, _params) = connection.initialize_start()?;
    let result = serde_json::json!({
        "capabilities": serde_json::to_value(server_capabilities())?,
    });
    connection
        .sender
        .send(Message::Response(Response::new_ok(id, result)))?;

    loop {
        match connection.receiver.recv()? {
            Message::Notification(n) if n.method == Initialized::METHOD => return Ok(()),
            // The client gave up before it finished starting us.
            Message::Notification(n) if n.method == Exit::METHOD => {
                return Err("client exited during initialization".into());
            }
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Err("client shut us down during initialization".into());
                }
                connection.sender.send(Message::Response(Response::new_err(
                    req.id,
                    lsp_server::ErrorCode::ServerNotInitialized as i32,
                    "the server is still initializing".to_string(),
                )))?;
            }
            _ => {}
        }
    }
}

fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        // Full sync: documents are small and analysis re-runs from scratch, so
        // applying incremental edits ourselves would buy nothing.
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(
            SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
                legend: SemanticTokensLegend {
                    token_types: tokens::LEGEND
                        .iter()
                        .map(|t| SemanticTokenType::new(t))
                        .collect(),
                    token_modifiers: vec![],
                },
                full: Some(SemanticTokensFullOptions::Bool(true)),
                ..Default::default()
            }),
        ),
        ..Default::default()
    }
}

struct Doc {
    text: String,
    analysis: Analysis,
    index: LineIndex,
}

struct Server {
    std: Std,
    docs: HashMap<Uri, Doc>,
    /// Line indexes for sources that are not open documents -- the modules a
    /// definition can land in. Keyed by source id, and never invalidated: see
    /// [`Server::range_in`].
    indexes: HashMap<u32, LineIndex>,
}

impl Server {
    fn main_loop(&mut self, c: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
        for msg in &c.receiver {
            match msg {
                Message::Request(req) => {
                    if c.handle_shutdown(&req)? {
                        return Ok(());
                    }
                    let response = self.request(req);
                    c.sender.send(Message::Response(response))?;
                }
                Message::Notification(note) => {
                    if let Some((uri, text)) = self.notification(note) {
                        let diagnostics = self.publish(&uri);
                        c.sender.send(Message::Notification(lsp_server::Notification {
                            method: PublishDiagnostics::METHOD.to_string(),
                            params: serde_json::to_value(PublishDiagnosticsParams {
                                uri,
                                diagnostics,
                                version: None,
                            })?,
                        }))?;
                        let _ = text;
                    }
                }
                Message::Response(_) => {}
            }
        }
        Ok(())
    }

    /// Returns the document to re-publish diagnostics for, if any.
    fn notification(&mut self, note: lsp_server::Notification) -> Option<(Uri, ())> {
        match note.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let p: DidOpenTextDocumentParams = serde_json::from_value(note.params).ok()?;
                self.set(p.text_document.uri.clone(), p.text_document.text);
                Some((p.text_document.uri, ()))
            }
            DidChangeTextDocument::METHOD => {
                let p: DidChangeTextDocumentParams = serde_json::from_value(note.params).ok()?;
                // Full sync, so the last change carries the whole document.
                let text = p.content_changes.into_iter().last()?.text;
                self.set(p.text_document.uri.clone(), text);
                Some((p.text_document.uri, ()))
            }
            DidCloseTextDocument::METHOD => {
                let p: DidCloseTextDocumentParams = serde_json::from_value(note.params).ok()?;
                self.docs.remove(&p.text_document.uri);
                None
            }
            _ => None,
        }
    }

    fn set(&mut self, uri: Uri, text: String) {
        // A file that *is* a `Std` module is analysed in its own place. Analysed
        // the ordinary way it would declare every one of its own types a second
        // time — once here, once in the `Std` it depends on — and the editor
        // would fill with `already defined`.
        let analysis = match self.std.module_at(uri.as_str()) {
            Some(i) => self.std.analyse_module(i, &text),
            None => self.std.analyse(&text),
        };
        let index = LineIndex::new(&text);
        self.docs.insert(
            uri,
            Doc {
                text,
                analysis,
                index,
            },
        );
    }

    fn publish(&self, uri: &Uri) -> Vec<Diagnostic> {
        let Some(doc) = self.docs.get(uri) else {
            return Vec::new();
        };
        doc.analysis
            .diagnostics
            .iter()
            .map(|d| {
                let (start, end) = doc.index.range(d.label.1);
                Diagnostic {
                    range: Range {
                        start: Position::new(start.0, start.1),
                        end: Position::new(end.0, end.1),
                    },
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("meadow".into()),
                    message: d.msg.clone(),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn request(&mut self, req: Request) -> Response {
        let id = req.id.clone();
        match req.method.as_str() {
            HoverRequest::METHOD => self.answer::<HoverRequest, _>(req, |s, p| {
                let (doc, offset) = s.at(&p.text_document_position_params)?;
                Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: doc.analysis.hover_at(offset)?,
                    }),
                    range: None,
                })
            }),
            GotoDefinition::METHOD => self.answer::<GotoDefinition, _>(req, |s, p| {
                let here = p.text_document_position_params.text_document.uri.clone();
                let (doc, offset) = s.at(&p.text_document_position_params)?;
                let loc = doc
                    .analysis
                    .definition_at(offset, s.std.definitions(), s.std.declared_names())?;

                // The definition may be in a file that is not open — so the
                // position has to be measured against *that* source's text, not
                // this document's. The two agree only in the case this used to
                // be able to answer.
                let mine = (loc.source.id == doc.analysis.source_id)
                    .then(|| doc.index.range(loc.span));

                let (uri, (start, end)) = match mine {
                    Some(range) => (here, range),
                    None => {
                        let path = s.std.path_of(loc.source)?;
                        (path_to_uri(&path)?, s.range_in(loc.source, loc.span))
                    }
                };
                Some(GotoDefinitionResponse::Scalar(Location {
                    uri,
                    range: Range {
                        start: Position::new(start.0, start.1),
                        end: Position::new(end.0, end.1),
                    },
                }))
            }),
            InlayHintRequest::METHOD => self.answer::<InlayHintRequest, _>(req, |s, p| {
                let from = {
                    let doc = s.docs.get(&p.text_document.uri)?;
                    doc.index.offset(p.range.start.line, p.range.start.character)
                };
                let to = {
                    let doc = s.docs.get(&p.text_document.uri)?;
                    doc.index.offset(p.range.end.line, p.range.end.character)
                };
                // Cloned out of the document because resolving a type name to
                // its declaration wants the whole server, and that file may not
                // be one this one is borrowed from.
                let wanted: Vec<_> = {
                    let doc = s.docs.get(&p.text_document.uri)?;
                    doc.analysis
                        .binders
                        .iter()
                        .filter(|h| (h.offset as usize) >= from && (h.offset as usize) <= to)
                        .cloned()
                        .collect()
                };

                let mut out = Vec::with_capacity(wanted.len());
                for hint in wanted {
                    let at = {
                        let doc = s.docs.get(&p.text_document.uri)?;
                        doc.index.position(hint.offset as usize)
                    };
                    let parts = s.label_parts(&p.text_document.uri, &hint.parts)?;
                    out.push(InlayHint {
                        position: Position::new(at.0, at.1),
                        label: InlayHintLabel::LabelParts(parts),
                        kind: Some(InlayHintKind::TYPE),
                        text_edits: None,
                        tooltip: None,
                        padding_left: None,
                        padding_right: None,
                        data: None,
                    });
                }
                Some(out)
            }),
            SemanticTokensFullRequest::METHOD => {
                self.answer::<SemanticTokensFullRequest, _>(req, |s, p| {
                    let doc = s.docs.get(&p.text_document.uri)?;
                    let raw = tokens::tokens(&doc.text, &doc.analysis);
                    Some(SemanticTokensResult::Tokens(SemanticTokens {
                        result_id: None,
                        data: tokens::encode(&raw)
                            .chunks(5)
                            .map(|c| SemanticToken {
                                delta_line: c[0],
                                delta_start: c[1],
                                length: c[2],
                                token_type: c[3],
                                token_modifiers_bitset: c[4],
                            })
                            .collect(),
                    }))
                })
            }
            _ => Response::new_err(
                id,
                lsp_server::ErrorCode::MethodNotFound as i32,
                format!("unhandled request: {}", req.method),
            ),
        }
    }

    /// Decode a request's parameters, run `f`, and encode what it returns.
    ///
    /// Every result type here is itself an `Option`, which is what lets the
    /// bodies use `?` freely: "nothing at this position" is a normal answer, and
    /// serialises to the `null` the protocol expects.
    fn answer<R, F>(&mut self, req: Request, f: F) -> Response
    where
        R: LspRequest,
        F: FnOnce(&mut Server, R::Params) -> R::Result,
    {
        let id: RequestId = req.id.clone();
        match cast::<R>(req) {
            Ok((id, params)) => Response::new_ok(id, f(self, params)),
            Err(e) => Response::new_err(
                id,
                lsp_server::ErrorCode::InvalidParams as i32,
                e.to_string(),
            ),
        }
    }

    fn at(&self, p: &TextDocumentPositionParams) -> Option<(&Doc, usize)> {
        let doc = self.docs.get(&p.text_document.uri)?;
        Some((doc, doc.index.offset(p.position.line, p.position.character)))
    }

    /// An inlay hint's label, with every type name in it linked.
    ///
    /// The link is what makes a hint navigable: `location` on a label part is
    /// how an editor offers ctrl-click and a hover on something that is not in
    /// the file at all. A name with nowhere to go -- a builtin like `Int`, or a
    /// type variable -- stays plain text rather than becoming a dead link.
    fn label_parts(
        &mut self,
        here: &Uri,
        parts: &[HintPart],
    ) -> Option<Vec<InlayHintLabelPart>> {
        let mut out = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                HintPart::Text(t) => out.push(plain(t.clone())),
                HintPart::Name(name) => {
                    let loc = {
                        let doc = self.docs.get(here)?;
                        doc.analysis
                            .names
                            .types
                            .get(name)
                            .copied()
                            .or_else(|| self.std.declared_names().types.get(name).copied())
                    };
                    let mut p = plain(name.to_string());
                    if let Some(loc) = loc {
                        p.location = self.locate(here, loc);
                        p.tooltip = Some(InlayHintLabelPartTooltip::String(format!(
                            "type `{name}`"
                        )));
                    }
                    out.push(p);
                }
            }
        }
        Some(out)
    }

    /// A [`Loc`] as a protocol `Location`, wherever it lives.
    fn locate(&mut self, here: &Uri, loc: Loc) -> Option<Location> {
        let mine = {
            let doc = self.docs.get(here)?;
            (loc.source.id == doc.analysis.source_id).then(|| doc.index.range(loc.span))
        };
        let (uri, (start, end)) = match mine {
            Some(range) => (here.clone(), range),
            None => {
                let path = self.std.path_of(loc.source)?;
                (path_to_uri(&path)?, self.range_in(loc.source, loc.span))
            }
        };
        Some(Location {
            uri,
            range: Range {
                start: Position::new(start.0, start.1),
                end: Position::new(end.0, end.1),
            },
        })
    }

    /// `span` as a line/character range within `source`, which need not be a
    /// document the editor has open.
    ///
    /// The line index is kept because the text it is built from never changes:
    /// these are dependencies, and an edit to one recompiles it into a new
    /// [`Source`] with a new id rather than mutating this one.
    fn range_in(&mut self, source: Source, span: Span) -> ((u32, u32), (u32, u32)) {
        self.indexes
            .entry(source.id)
            .or_insert_with(|| LineIndex::new(&source.content))
            .range(span)
    }
}

/// A `file://` URI for a path on this machine.
///
/// Hand-rolled because the alternative is a dependency, and because the two
/// things that go wrong here are worth being explicit about: a Windows path
/// needs its separators flipped and a leading slash before the drive letter,
/// and anything outside the unreserved set has to be percent-encoded — an
/// install under a user whose name contains a space is not exotic.
fn path_to_uri(path: &std::path::Path) -> Option<Uri> {
    let text = path.to_str()?.replace('\\', "/");
    let mut out = String::from("file://");
    if !text.starts_with('/') {
        out.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out.parse().ok()
}

fn cast<R: LspRequest>(req: Request) -> Result<(RequestId, R::Params), ExtractError<Request>> {
    req.extract(R::METHOD).map(|(id, p)| (id, p))
}

/// A label part with nothing attached — the common case, and the base every
/// other one is built from.
fn plain(value: String) -> InlayHintLabelPart {
    InlayHintLabelPart {
        value,
        tooltip: None,
        location: None,
        command: None,
    }
}

//! The protocol loop: `meadow lsp` speaks LSP over stdin/stdout.
//!
//! Synchronous and single-threaded on purpose. A request is answered by
//! re-analysing the document it names, and analysis is fast enough that a queue
//! and a cancellation protocol would be more machinery than the problem needs.
//! `Std` is compiled once at startup, which is the only slow part.

use crate::analysis::{Analysis, Std};
use crate::pos::LineIndex;
use crate::tokens;
use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use meadow_compiler::CompiledPackage;
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification,
    PublishDiagnostics,
};
use lsp_types::request::{
    GotoDefinition, HoverRequest, InlayHintRequest, Request as LspRequest,
    SemanticTokensFullRequest,
};
use lsp_types::*;
use std::collections::HashMap;
use std::error::Error;

pub fn run(std_packages: Vec<CompiledPackage>) -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    serve(&connection, std_packages)?;
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
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let capabilities = serde_json::to_value(server_capabilities())?;
    connection.initialize(capabilities)?;
    let mut server = Server {
        std: Std::new(std_packages),
        docs: HashMap::new(),
    };
    server.main_loop(connection)
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
        let analysis = self.std.analyse(&text);
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
                let (doc, offset) = s.at(&p.text_document_position_params)?;
                let span = doc.analysis.definition_at(offset)?;
                let (start, end) = doc.index.range(span);
                Some(GotoDefinitionResponse::Scalar(Location {
                    uri: p.text_document_position_params.text_document.uri.clone(),
                    range: Range {
                        start: Position::new(start.0, start.1),
                        end: Position::new(end.0, end.1),
                    },
                }))
            }),
            InlayHintRequest::METHOD => self.answer::<InlayHintRequest, _>(req, |s, p| {
                let doc = s.docs.get(&p.text_document.uri)?;
                let from = doc.index.offset(p.range.start.line, p.range.start.character);
                let to = doc.index.offset(p.range.end.line, p.range.end.character);
                Some(
                    doc.analysis
                        .binders
                        .iter()
                        .filter(|(span, _)| {
                            (span.start as usize) >= from && (span.end as usize) <= to
                        })
                        .map(|(span, ty)| {
                            let (_, end) = doc.index.range(*span);
                            InlayHint {
                                position: Position::new(end.0, end.1),
                                label: InlayHintLabel::String(format!(" : {ty}")),
                                kind: Some(InlayHintKind::TYPE),
                                text_edits: None,
                                tooltip: None,
                                padding_left: None,
                                padding_right: None,
                                data: None,
                            }
                        })
                        .collect::<Vec<_>>(),
                )
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
}

fn cast<R: LspRequest>(req: Request) -> Result<(RequestId, R::Params), ExtractError<Request>> {
    req.extract(R::METHOD).map(|(id, p)| (id, p))
}

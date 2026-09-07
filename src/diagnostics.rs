//! Diagnostics.
//!
//! Every pass reports problems as a plain [`Diagnostic`] (message + primary label
//! + optional extra labels, all carrying [`Span`]s). Passes collect these rather
//! than bailing, so one compile surfaces every error it can. [`emit`] renders a
//! batch to stderr with `ariadne` (source snippet, carets, colour); tests just
//! read [`Diagnostic::msg`].
//!
//! [`Span`]: crate::span::Span

use crate::{lexer::Token, span::Span};
use ariadne::{Color, Label, Report, ReportKind};
use chumsky::error::Rich;
use std::ops::Range;

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub msg: String,
    pub filename: String,
    pub label: (String, Span),
    pub extra_labels: Vec<(String, Span)>,
}

impl Diagnostic {
    pub fn new(
        msg: String,
        filename: String,
        label: (String, Span),
        extra_labels: Vec<(String, Span)>,
    ) -> Self {
        Self {
            msg,
            filename,
            label,
            extra_labels,
        }
    }
}

pub fn parse_report<'src>(
    msg: String,
    filename: String,
    label: (String, Span),
    err: &Rich<'src, Token, Span>,
) -> Report<'src, (String, Range<usize>)> {
    build_report(Diagnostic {
        msg,
        filename,
        label,
        extra_labels: err
            .contexts()
            .map(|(l, s)| (format!("while parsing this {l}"), *s))
            .collect(),
    })
}

pub fn build_report<'src>(diag: Diagnostic) -> Report<'src, (String, Range<usize>)> {
    Report::build(
        ReportKind::Error,
        (diag.filename.clone(), Range::from(diag.label.1)),
    )
    .with_config(ariadne::Config::default())
    .with_message(diag.msg)
    .with_label(
        Label::new((diag.filename.clone(), Range::from(diag.label.1)))
            .with_message(diag.label.0)
            .with_color(Color::Red),
    )
    .with_labels(diag.extra_labels.into_iter().map(|label2| {
        Label::new((diag.filename.clone(), Range::from(label2.1)))
            .with_message(label2.0)
            .with_color(Color::Yellow)
    }))
    .finish()
}

/// Convert a `chumsky` parse error into a [`Diagnostic`].
pub fn from_parse_error(filename: &str, e: &Rich<'_, Token, Span>) -> Diagnostic {
    Diagnostic {
        msg: e.to_string(),
        filename: filename.to_string(),
        label: (e.reason().to_string(), *e.span()),
        extra_labels: e
            .contexts()
            .map(|(l, s)| (format!("while parsing this {l}"), *s))
            .collect(),
    }
}

/// Pretty-print a batch of diagnostics to stderr with `ariadne`.
///
/// Every diagnostic is assumed to point into the single source `text` (true for
/// one REPL entry or one [`crate::pipeline::compile_str`] call). `label` is the
/// name shown in each report header; it also becomes the span id, so the source
/// snippet always resolves.
pub fn emit(diags: &[Diagnostic], label: &str, text: &str) {
    if diags.is_empty() {
        return;
    }
    let source = ariadne::Source::from(text.to_string());
    for d in diags {
        let mut d = d.clone();
        d.filename = label.to_string();
        let report = build_report(d);
        let _ = report.eprint((label.to_string(), source.clone()));
    }
}

use crate::{span::Span, lexer::Token};
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

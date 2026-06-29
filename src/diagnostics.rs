use std::ops::Range;

use ariadne::{Color, Label, Report, ReportKind};
use chumsky::error::Rich;

use crate::{span::Span, token::Token};

pub fn parse_report<'src>(
    msg: String,
    filename: String,
    label: (String, Span),
    err: &Rich<'src, Token, Span>,
) -> Report<'src, (String, Range<usize>)> {
    build_report(
        msg,
        filename,
        label,
        err.contexts()
            .map(|(l, s)| (format!("while parsing this {l}"), *s)),
    )
}

fn build_report<'src>(
    msg: String,
    filename: String,
    label: (String, Span),
    extra_labels: impl IntoIterator<Item = (String, Span)>,
) -> Report<'src, (String, Range<usize>)> {
    Report::build(ReportKind::Error, (filename.clone(), Range::from(label.1)))
        .with_config(ariadne::Config::default())
        .with_message(msg)
        .with_label(
            Label::new((filename.clone(), Range::from(label.1)))
                .with_message(label.0)
                .with_color(Color::Red),
        )
        .with_labels(extra_labels.into_iter().map(|label2| {
            Label::new((filename.clone(), Range::from(label2.1)))
                .with_message(label2.0)
                .with_color(Color::Yellow)
        }))
        .finish()
}

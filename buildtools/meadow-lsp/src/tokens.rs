//! Semantic tokens, derived from the lexer.
//!
//! Highlighting from the compiler's own lexer cannot drift from the language the
//! way a hand-written grammar can. It also settles what a grammar can only guess:
//! `Maybe` and `Just` are both capitalised, and only name resolution knows that
//! one is a type and the other a constructor.
//!
//! The extension still ships a TextMate grammar — it colours a file before the
//! server has started, and comments never reach the lexer at all.

use crate::analysis::Analysis;
use crate::pos::LineIndex;
use meadow_compiler::lexer::{tokenize, Token};
use meadow_compiler::source::{Source, SourceKind};

/// The legend, in the order the protocol indexes it.
pub const LEGEND: &[&str] = &[
    "keyword",
    "type",
    "enumMember",
    "function",
    "variable",
    "number",
    "string",
    "operator",
    "namespace",
];

fn index_of(name: &str) -> u32 {
    LEGEND.iter().position(|l| *l == name).unwrap_or(4) as u32
}

/// `(line, start character, length, token type)`, in source order.
pub fn tokens(text: &str, analysis: &Analysis) -> Vec<(u32, u32, u32, u32)> {
    let source = Source::new(SourceKind::Interactive, text.into());
    let lex = tokenize(source);
    let idx = LineIndex::new(text);
    let mut out = Vec::new();

    for t in &lex.tokens {
        let kind = match t.value() {
            Token::Mod
            | Token::Use
            | Token::Def
            | Token::Fun
            | Token::Let
            | Token::In
            | Token::Match
            | Token::With
            | Token::If
            | Token::Then
            | Token::Else
            | Token::Data
            | Token::Record
            | Token::Effect
            | Token::Handle
            | Token::Type
            | Token::Class
            | Token::End
            | Token::As
            | Token::And
            | Token::Or => "keyword",
            Token::Int(_) | Token::Real(_) => "number",
            Token::String(_) | Token::Char(_) => "string",
            Token::UpperIdent(name) => {
                let n = name.to_string();
                if analysis.ctors_in_scope.contains(&n) {
                    "enumMember"
                } else if analysis.types_in_scope.contains(&n) {
                    "type"
                } else {
                    // An unresolved capital is a module qualifier or a name the
                    // file has not defined yet; `namespace` reads better than
                    // guessing `type`.
                    "namespace"
                }
            }
            Token::LowerIdent(_) => "variable",
            Token::OpIdent(_)
            | Token::ConOpIdent(_)
            | Token::Plus
            | Token::Minus
            | Token::Star
            | Token::Slash
            | Token::Percent
            | Token::Caret
            | Token::Eq
            | Token::EqEq
            | Token::Neq
            | Token::Lt
            | Token::Gt
            | Token::Leq
            | Token::Geq
            | Token::Bang
            | Token::RArrow
            | Token::LArrow
            | Token::ColonColon
            | Token::LPipe
            | Token::RPipe
            | Token::Backslash
            | Token::At => "operator",
            _ => continue,
        };

        let (line, start) = idx.position(t.span.start as usize);
        let (end_line, end) = idx.position(t.span.end as usize);
        // A token that wraps a line would need splitting; none of ours do, and
        // emitting a bad length would corrupt every token after it.
        if line != end_line {
            continue;
        }
        out.push((line, start, end - start, index_of(kind)));
    }
    out
}

/// The protocol wants each token relative to the one before it.
pub fn encode(tokens: &[(u32, u32, u32, u32)]) -> Vec<u32> {
    let mut data = Vec::with_capacity(tokens.len() * 5);
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for &(line, start, len, kind) in tokens {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 {
            start - prev_start
        } else {
            start
        };
        data.extend_from_slice(&[delta_line, delta_start, len, kind, 0]);
        prev_line = line;
        prev_start = start;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deltas_are_relative_to_the_previous_token() {
        let toks = vec![(0, 0, 3, 0), (0, 4, 2, 1), (2, 5, 1, 2)];
        assert_eq!(
            encode(&toks),
            vec![0, 0, 3, 0, 0, 0, 4, 2, 1, 0, 2, 5, 1, 2, 0]
        );
    }
}

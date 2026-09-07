//! Lexing, via `logos`.
//!
//! [`tokenize`] turns a [`Source`] into a `Vec<LToken>` (each a `Token` + its
//! [`Span`]). Whitespace and `--` line comments are skipped by the lexer itself.
//! An unrecognized byte becomes a [`Token::Error`] *and* a [`Diagnostic`], so the
//! parser can keep going.
//!
//! Note the operator handling: `==`, `->`, `<=` … are their own `#[token]`s and
//! win over the catch-all `OpIdent` regex (which covers user-defined operator
//! names); this is why the regex must *not* include letters.

use crate::{
    diagnostics::Diagnostic,
    intern::InternedString,
    source::Source,
    span::{Located, Span},
};
use logos::Logos;
use std::fmt::Display;

pub type LToken = Located<Token>;

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(subpattern alpha = r"[a-zA-Z]+")]
pub enum Token {
    Eof,
    #[regex(r"--[^\n]*", logos::skip, allow_greedy = true)]
    Comment,
    #[regex(r"[ \t\n\r]+", logos::skip)]
    Whitespace,
    // Literals and identifiers
    #[regex(
        r"-?((0b[0-1]+)|(0o[0-7]+)|(0x[0-9a-fA-F]+)|([1-9]\d*|0))", 
        |lex| lex.slice().parse().ok(),
        priority = 3
    )]
    Int(i64),
    #[regex(
        r"([0-9]*[.])?[0-9]+", 
        |lex| lex.slice().parse().ok(),
        priority = 2
    )]
    Real(f64),
    // #[regex(
    //     r"-?((0b[0-1]+)|(0o[0-7]+)|(0x[0-9a-fA-F]+)|([1-9]\d*|0))(/-?((0b[0-1]+)|(0o[0-7]+)|(0x[0-9a-fA-F]+)|([1-9]\d*|0)))",
    //     |lex| lex.slice().parse().ok())]
    // Rational(Rational64),
    #[regex(r#""(\\.|[^"\\])*""#, |lex| InternedString::from(lex.slice()))]
    String(InternedString),
    #[regex(r"'(\\.|[^'\\])'", |lex| lex.slice().chars().nth(1))]
    Char(char),
    #[regex(r"[a-z][a-zA-Z0-9'_]*", |lex| InternedString::from(lex.slice()), priority = 2)]
    LowerIdent(InternedString),
    #[regex(r"[A-Z][a-zA-Z0-9']*", |lex| InternedString::from(lex.slice()))]
    UpperIdent(InternedString),
    #[regex(r"[!$%&*+./<=>?@|^~:\-]+", |lex| InternedString::from(lex.slice()), priority = 1)]
    OpIdent(InternedString),
    #[regex(r":[!$%&*+./<=>?@|^~:\-]+", |lex| InternedString::from(lex.slice()))]
    ConOpIdent(InternedString),

    // Punctuation
    #[token("_")]
    Wildcard,
    #[token("\\")]
    Backslash,
    #[token("<-")]
    LArrow,
    #[token("->")]
    RArrow,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("^")]
    Caret,
    #[token("or")]
    Or,
    #[token("and")]
    And,
    #[token("not")]
    Not,
    #[token("=")]
    Eq,
    #[token("==")]
    EqEq,
    #[token("!=")]
    Neq,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("<=")]
    Leq,
    #[token(">=")]
    Geq,
    #[token("!")]
    Bang,
    #[token(",")]
    Comma,
    #[token(".")]
    Period,
    #[token("..")]
    DoublePeriod,
    #[token("..=")]
    DoublePeriodEq,
    #[token(":")]
    Colon,
    #[token(";")]
    SemiColon,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBrack,
    #[token("]")]
    RBrack,
    #[token("#")]
    Hash,
    #[token("|")]
    Bar,
    #[token("<|")]
    LPipe,
    #[token("|>")]
    RPipe,
    #[token("@")]
    At,
    #[token("`")]
    Backtick,

    // Keywords
    #[token("mod")]
    Mod,
    #[token("end")]
    End,
    #[token("use")]
    Use,
    #[token("def")]
    Def,
    #[token("fun")]
    Fun,
    #[token("let")]
    Let,
    #[token("in")]
    In,
    #[token("match")]
    Match,
    #[token("with")]
    With,
    #[token("if")]
    If,
    #[token("then")]
    Then,
    #[token("else")]
    Else,
    #[token("data")]
    Data,
    #[token("record")]
    Record,
    #[token("effect")]
    Effect,
    #[token("handle")]
    Handle,
    #[token("type")]
    Type,
    #[token("class")]
    Class,
    #[token("instance")]
    Instance,
    #[token("as")]
    As,
    Error,
}

impl<'a> Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Token::*;
        match self {
            Eof => write!(f, "Eof"),
            Comment => write!(f, "Comment"),
            Whitespace => write!(f, "Whitespace"),
            Int(i) => write!(f, "Int({})", i),
            Real(r) => write!(f, "Real({})", r),
            String(s) => write!(f, "InternedString({})", s),
            Char(c) => write!(f, "Char({})", c),
            LowerIdent(s) => write!(f, "LowerIdent({})", s),
            UpperIdent(s) => write!(f, "UpperIdent({})", s),
            OpIdent(s) => write!(f, "OpIdent({})", s),
            ConOpIdent(s) => write!(f, "ConOpIdent({})", s),

            Wildcard => write!(f, "Wildcard"),
            Backslash => write!(f, "Backslash"),
            LArrow => write!(f, "LArrow"),
            RArrow => write!(f, "RArrow"),
            Plus => write!(f, "Plus"),
            Minus => write!(f, "Minus"),
            Star => write!(f, "Star"),
            Slash => write!(f, "Slash"),
            Percent => write!(f, "Percent"),
            Caret => write!(f, "Caret"),
            Or => write!(f, "Or"),
            And => write!(f, "And"),
            Not => write!(f, "Not"),
            Eq => write!(f, "Eq"),
            EqEq => write!(f, "EqEq"),
            Neq => write!(f, "Neq"),
            Lt => write!(f, "Lt"),
            Gt => write!(f, "Gt"),
            Leq => write!(f, "Leq"),
            Geq => write!(f, "Geq"),
            Bang => write!(f, "Bang"),
            Comma => write!(f, "Comma"),
            Period => write!(f, "Period"),
            DoublePeriod => write!(f, "DoublePeriod"),
            DoublePeriodEq => write!(f, "DoublePeriodEq"),
            Colon => write!(f, "Colon"),
            SemiColon => write!(f, "SemiColon"),
            LParen => write!(f, "LParen"),
            RParen => write!(f, "RParen"),
            LBrace => write!(f, "LBrace"),
            RBrace => write!(f, "RBrace"),
            LBrack => write!(f, "LBrack"),
            RBrack => write!(f, "RBrack"),
            Hash => write!(f, "HashLBrack"),
            Bar => write!(f, "Bar"),
            LPipe => write!(f, "LPipe"),
            RPipe => write!(f, "RPipe"),
            At => write!(f, "At"),
            Backtick => write!(f, "Backtick"),

            Mod => write!(f, "Mod"),
            End => write!(f, "End"),
            Use => write!(f, "Use"),
            Def => write!(f, "Def"),
            Fun => write!(f, "Fun"),
            Let => write!(f, "Let"),
            In => write!(f, "In"),
            If => write!(f, "If"),
            Match => write!(f, "Match"),
            With => write!(f, "With"),
            Then => write!(f, "Then"),
            Else => write!(f, "Else"),
            Data => write!(f, "Data"),
            Record => write!(f, "Record"),
            Effect => write!(f, "Effect"),
            Handle => write!(f, "Handle"),
            Type => write!(f, "Type"),
            Class => write!(f, "Class"),
            Instance => write!(f, "Instance"),
            As => write!(f, "As"),
            Error => write!(f, "Error"),
        }
    }
}

pub struct LexResult {
    pub tokens: Vec<LToken>,
    pub errors: Vec<Diagnostic>,
}

pub fn tokenize(src: Source) -> LexResult {
    let mut lexer = Token::lexer(&src.content);
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    while let Some(res) = lexer.next() {
        match res {
            Ok(token) => {
                let span = Span::from(lexer.span());
                tokens.push(LToken::new(token, span));
            }
            Err(_) => {
                let span = Span::from(lexer.span());
                let err = Diagnostic::new(
                    format!("Invalid token: {}", &src[span]),
                    src.name().to_string(),
                    ("Invalid token".to_string(), span),
                    vec![],
                );
                errors.push(err);
                tokens.push(LToken::new(Token::Error, span));
            }
        }
    }
    LexResult { tokens, errors }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceKind;

    /// Lex `src` and return just the token kinds (spans and errors dropped).
    fn kinds(src: &str) -> Vec<Token> {
        let source = Source::new(SourceKind::Interactive, src.into());
        tokenize(source)
            .tokens
            .into_iter()
            .map(|t| *t.value)
            .collect()
    }

    #[test]
    fn keywords_and_identifiers() {
        use Token::*;
        assert_eq!(
            kinds("fun map xs"),
            vec![Fun, LowerIdent("map".into()), LowerIdent("xs".into())]
        );
    }

    #[test]
    fn upper_vs_lower_identifiers() {
        use Token::*;
        assert_eq!(
            kinds("Cons x"),
            vec![UpperIdent("Cons".into()), LowerIdent("x".into())]
        );
    }

    #[test]
    fn operators_are_distinct_tokens_not_op_idents() {
        // `==` and `->` must win over the generic `OpIdent` regex.
        use Token::*;
        assert_eq!(kinds("a == b"), vec![LowerIdent("a".into()), EqEq, LowerIdent("b".into())]);
        assert_eq!(kinds("\\x -> x"), vec![Backslash, LowerIdent("x".into()), RArrow, LowerIdent("x".into())]);
    }

    #[test]
    fn comments_and_whitespace_are_skipped() {
        use Token::*;
        assert_eq!(kinds("1 -- a comment\n+ 2"), vec![Int(1), Plus, Int(2)]);
    }

    #[test]
    fn numbers_and_strings() {
        use Token::*;
        // string literals keep their surrounding quotes in the slice
        assert_eq!(kinds("42 \"hi\""), vec![Int(42), String("\"hi\"".into())]);
    }

    #[test]
    fn record_is_a_keyword() {
        assert_eq!(kinds("record"), vec![Token::Record]);
    }

    #[test]
    fn invalid_character_reports_an_error() {
        let res = tokenize(Source::new(SourceKind::Interactive, "a \0 b".into()));
        assert!(!res.errors.is_empty());
    }
}

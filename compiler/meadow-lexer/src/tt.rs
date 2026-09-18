//! Token trees: a token, or a bracketed run of token trees.
//!
//! This is the shape a macro sees ([`docs/MACROS.md`]). Grouping by bracket is
//! all the structure a macro's argument is required to have -- it need not be an
//! expression, or parse at all -- so [`trees`] is the one thing standing between
//! the lexer and a macro call, and the only error it can report is a bracket
//! that does not match.
//!
//! [`flatten`] is the inverse, and the two round-trip exactly: the tokens that
//! come out are the tokens that went in, in order, with their spans. That is
//! what lets expansion hand its result back to the ordinary parser.
//!
//! [`docs/MACROS.md`]: https://github.com/mcdearman/meadow/blob/master/docs/MACROS.md

use crate::{LToken, Token};
use meadow_diagnostics::Diagnostic;
use meadow_span::Span;
use std::fmt::Display;

/// Which pair of brackets a [`Group`] is written with. The three mean the same
/// thing to a macro; which one to use is a question of how the call reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Delim {
    /// `( … )`
    Paren,
    /// `[ … ]`
    Brack,
    /// `{ … }`
    Brace,
}

impl Delim {
    /// The delimiter `t` opens, if it opens one.
    pub fn opened_by(t: &Token) -> Option<Self> {
        match t {
            Token::LParen => Some(Delim::Paren),
            Token::LBrack => Some(Delim::Brack),
            Token::LBrace => Some(Delim::Brace),
            _ => None,
        }
    }

    /// The delimiter `t` closes, if it closes one.
    pub fn closed_by(t: &Token) -> Option<Self> {
        match t {
            Token::RParen => Some(Delim::Paren),
            Token::RBrack => Some(Delim::Brack),
            Token::RBrace => Some(Delim::Brace),
            _ => None,
        }
    }

    /// The token that opens this delimiter.
    pub fn open(self) -> Token {
        match self {
            Delim::Paren => Token::LParen,
            Delim::Brack => Token::LBrack,
            Delim::Brace => Token::LBrace,
        }
    }

    /// The token that closes this delimiter.
    pub fn close(self) -> Token {
        match self {
            Delim::Paren => Token::RParen,
            Delim::Brack => Token::RBrack,
            Delim::Brace => Token::RBrace,
        }
    }
}

impl Display for Delim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Delim::Paren => write!(f, "()"),
            Delim::Brack => write!(f, "[]"),
            Delim::Brace => write!(f, "{{}}"),
        }
    }
}

/// A bracketed run of token trees, and where its brackets are.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Group {
    pub delim: Delim,
    pub trees: Vec<TokenTree>,
    /// The opening bracket.
    pub open: Span,
    /// The closing bracket. For an unclosed group this is the empty span where
    /// the closing bracket should have been, so that a span built from it still
    /// points somewhere sensible.
    pub close: Span,
}

impl Group {
    /// The whole group, brackets included.
    pub fn span(&self) -> Span {
        self.open.extend(self.close)
    }
}

/// A token, or a bracketed run of token trees.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TokenTree {
    Token(LToken),
    Group(Group),
}

impl TokenTree {
    /// Where this tree was written.
    pub fn span(&self) -> Span {
        match self {
            TokenTree::Token(t) => t.span,
            TokenTree::Group(g) => g.span(),
        }
    }
}

/// Group `tokens` by bracket.
///
/// `name` names the source, for diagnostics. A bracket that is never closed, or
/// one that closes nothing (or closes the wrong thing), is reported; the tree is
/// still built, so that everything else in the file is still there to be parsed.
pub fn trees(tokens: &[LToken], name: &str) -> (Vec<TokenTree>, Vec<Diagnostic>) {
    let mut errors = Vec::new();
    let mut stack = vec![Frame {
        open: None,
        trees: Vec::new(),
    }];
    let push = |stack: &mut Vec<Frame>, tree| {
        stack
            .last_mut()
            .expect("the top level is never popped")
            .trees
            .push(tree);
    };

    for t in tokens {
        if let Some(d) = Delim::opened_by(t.value()) {
            stack.push(Frame {
                open: Some((d, t.span)),
                trees: Vec::new(),
            });
        } else if let Some(d) = Delim::closed_by(t.value()) {
            match stack.last().expect("the top level is never popped").open {
                // A group is open. If this is not the bracket that closes it --
                // `( … ]` -- say so, but close it anyway: the rest of the file
                // is more likely to be read right that way than by treating the
                // `]` as stray.
                Some((got, at)) => {
                    if got != d {
                        errors.push(mismatched(name, got, at, d, t.span));
                    }
                    let frame = stack.pop().expect("just looked at it");
                    push(
                        &mut stack,
                        TokenTree::Group(Group {
                            delim: got,
                            trees: frame.trees,
                            open: at,
                            close: t.span,
                        }),
                    );
                }
                // Nothing is open. Keep the token: the parser will say what it
                // makes of it, which is usually the clearer message.
                None => {
                    errors.push(Diagnostic::new(
                        format!("`{}` closes nothing", text(t.value())),
                        name.to_string(),
                        ("no bracket is open here".to_string(), t.span),
                        vec![],
                    ));
                    push(&mut stack, TokenTree::Token(t.clone()));
                }
            }
        } else {
            push(&mut stack, TokenTree::Token(t.clone()));
        }
    }

    // Whatever is still open was never closed. Close each where the input ran
    // out, innermost first, so its contents are not lost.
    let eof = tokens
        .last()
        .map_or(Span::new(0, 0), |t| Span::new(t.span.end, t.span.end));
    while stack.len() > 1 {
        let frame = stack.pop().expect("more than one frame");
        let (d, at) = frame.open.expect("only the top level has none");
        errors.push(Diagnostic::new(
            format!("this `{}` is never closed", text(&d.open())),
            name.to_string(),
            (format!("expected a `{}` for this", text(&d.close())), at),
            vec![],
        ));
        push(
            &mut stack,
            TokenTree::Group(Group {
                delim: d,
                trees: frame.trees,
                open: at,
                close: eof,
            }),
        );
    }
    (stack.pop().expect("the top level").trees, errors)
}

/// What [`trees`] is filling, innermost last. The first frame is the top level,
/// which no bracket opened; the rest each remember the bracket that did, so that
/// an unclosed one can be reported where it was written.
struct Frame {
    open: Option<(Delim, Span)>,
    trees: Vec<TokenTree>,
}

fn mismatched(name: &str, got: Delim, at: Span, closed: Delim, by: Span) -> Diagnostic {
    Diagnostic::new(
        format!(
            "`{}` does not close `{}`",
            text(&closed.close()),
            text(&got.open())
        ),
        name.to_string(),
        (format!("expected a `{}` for this", text(&got.close())), at),
        vec![(format!("`{}` is here", text(&closed.close())), by)],
    )
}

/// How a bracket is written, for a message.
fn text(t: &Token) -> &'static str {
    match t {
        Token::LParen => "(",
        Token::RParen => ")",
        Token::LBrack => "[",
        Token::RBrack => "]",
        Token::LBrace => "{",
        Token::RBrace => "}",
        _ => "?",
    }
}

/// The tokens of `trees`, brackets included: exactly what [`trees`] was given,
/// as long as every bracket matched.
pub fn flatten(trees: &[TokenTree]) -> Vec<LToken> {
    let mut out = Vec::new();
    flatten_into(trees, &mut out);
    out
}

fn flatten_into(trees: &[TokenTree], out: &mut Vec<LToken>) {
    for t in trees {
        match t {
            TokenTree::Token(t) => out.push(t.clone()),
            TokenTree::Group(g) => {
                out.push(LToken::new(g.delim.open(), g.open));
                flatten_into(&g.trees, out);
                out.push(LToken::new(g.delim.close(), g.close));
            }
        }
    }
}

/// Write `trees` back as source text: what `stringify!` answers.
///
/// The spacing is normalised rather than remembered -- a token tree does not
/// know what whitespace it was written with -- so tokens are separated by a
/// space except around punctuation that reads wrong with one: inside brackets,
/// before `,` `;` `.`, and either side of a `.`. What comes out lexes back to
/// the tokens that went in.
pub fn render(trees: &[TokenTree]) -> String {
    let mut out = String::new();
    render_into(trees, &mut out);
    out
}

fn render_into(trees: &[TokenTree], out: &mut String) {
    for t in trees {
        match t {
            TokenTree::Token(t) => {
                separate(t.value(), out);
                out.push_str(&t.value().text());
            }
            TokenTree::Group(g) => {
                separate(&g.delim.open(), out);
                out.push_str(&g.delim.open().text());
                render_into(&g.trees, out);
                out.push_str(&g.delim.close().text());
            }
        }
    }
}

/// Put a space before `next` if what is already written wants one.
fn separate(next: &Token, out: &mut String) {
    let Some(last) = out.chars().last() else {
        return;
    };
    let tight_before = matches!(
        next,
        Token::Comma
            | Token::SemiColon
            | Token::Period
            | Token::RParen
            | Token::RBrack
            | Token::RBrace
            // The closing half of an interpolated literal carries its own `}`.
            | Token::InterpMid(_)
            | Token::InterpEnd(_)
    );
    let tight_after = matches!(last, '(' | '[' | '{' | '.');
    if !tight_before && !tight_after {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenize;
    use meadow_source::{Source, SourceKind};

    fn lex(text: &str) -> Vec<LToken> {
        tokenize(Source::new(SourceKind::Interactive, text.into())).tokens
    }

    fn built(text: &str) -> (Vec<TokenTree>, Vec<Diagnostic>) {
        trees(&lex(text), "test")
    }

    #[test]
    fn brackets_nest() {
        let (ts, errs) = built("f (g [1; 2]) { x = 3 }");
        assert!(errs.is_empty());
        // `f`, the parenthesised argument, the braced record.
        assert_eq!(ts.len(), 3);
        let TokenTree::Group(g) = &ts[1] else {
            panic!("expected a group, got {:?}", ts[1]);
        };
        assert_eq!(g.delim, Delim::Paren);
        // `g` and the bracketed list.
        assert_eq!(g.trees.len(), 2);
        let TokenTree::Group(inner) = &g.trees[1] else {
            panic!("expected a group, got {:?}", g.trees[1]);
        };
        assert_eq!(inner.delim, Delim::Brack);
    }

    #[test]
    fn flattening_gives_back_what_was_lexed() {
        let src = "fun f (x : Int) : Int = match x with | [a; b] -> { n = a } | _ -> { n = 0 }";
        let tokens = lex(src);
        let (ts, errs) = trees(&tokens, "test");
        assert!(errs.is_empty());
        assert_eq!(flatten(&ts), tokens);
    }

    #[test]
    fn a_bracket_that_is_never_closed_is_reported_and_still_holds_its_tokens() {
        let (ts, errs) = built("f (g 1");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("never closed"), "{}", errs[0].msg);
        let TokenTree::Group(g) = &ts[1] else {
            panic!("expected a group, got {:?}", ts[1]);
        };
        assert_eq!(g.trees.len(), 2);
    }

    #[test]
    fn a_bracket_that_closes_nothing_is_reported_and_kept() {
        let (ts, errs) = built("f x )");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("closes nothing"), "{}", errs[0].msg);
        assert_eq!(ts.len(), 3);
    }

    #[test]
    fn the_wrong_closing_bracket_closes_the_group_anyway() {
        let (ts, errs) = built("f (g 1] h");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].msg.contains("does not close"), "{}", errs[0].msg);
        // `f`, the group the `]` closed, and `h` after it.
        assert_eq!(ts.len(), 3);
        let TokenTree::Group(g) = &ts[1] else {
            panic!("expected a group, got {:?}", ts[1]);
        };
        assert_eq!(g.delim, Delim::Paren);
    }

    #[test]
    fn a_string_hole_is_not_a_brace_group() {
        // The `${` and `}` of an interpolation are part of the string's own
        // tokens, so they neither open nor close a group -- what is inside the
        // hole sits in the stream on its own.
        let (ts, errs) = built(r#""a ${ f (1) } b""#);
        assert!(errs.is_empty());
        // InterpStart, `f`, the group, InterpEnd.
        assert_eq!(ts.len(), 4);
        assert!(matches!(&ts[2], TokenTree::Group(g) if g.delim == Delim::Paren));
    }

    #[test]
    fn a_dollar_is_its_own_token() {
        let tokens = lex("$x $( $a ),* $$");
        let kinds: Vec<_> = tokens.iter().map(|t| t.value().clone()).collect();
        assert_eq!(kinds[0], Token::Dollar);
        assert!(matches!(kinds[1], Token::LowerIdent(_)));
        assert_eq!(kinds[2], Token::Dollar);
        assert_eq!(kinds[3], Token::LParen);
        // `$$` is two of them, not one operator.
        assert_eq!(kinds[kinds.len() - 2], Token::Dollar);
        assert_eq!(kinds[kinds.len() - 1], Token::Dollar);
    }

    #[test]
    fn a_splice_inside_a_string_hole_is_a_dollar_in_the_holes_tokens() {
        // `"value: ${$x}"` -- the string's own `${` starts a hole, and what is
        // inside it is tokens again, so the splice is an ordinary `$` there.
        // This is what lets a template interpolate a fragment.
        let tokens = lex(r#""value: ${$x}""#);
        let kinds: Vec<_> = tokens.iter().map(|t| t.value().clone()).collect();
        assert!(matches!(kinds[0], Token::InterpStart(_)), "{kinds:?}");
        assert_eq!(kinds[1], Token::Dollar);
        assert!(matches!(kinds[2], Token::LowerIdent(_)), "{kinds:?}");
        assert!(matches!(kinds[3], Token::InterpEnd(_)), "{kinds:?}");
    }

    #[test]
    fn a_dollar_does_not_join_the_operator_beside_it() {
        // `),*` after a repetition: the `,` and `*` are still what they were,
        // and the `$` before the `(` did not swallow anything.
        let tokens = lex("$(x),*");
        let kinds: Vec<_> = tokens.iter().map(|t| t.value().clone()).collect();
        assert_eq!(kinds[0], Token::Dollar);
        assert_eq!(kinds[4], Token::Comma);
        assert_eq!(kinds[5], Token::Star);
    }
}

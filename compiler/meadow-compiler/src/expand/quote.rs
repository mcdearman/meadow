//! **`quote! { … }`**: tokens, written as tokens, with holes.
//!
//! A procedural macro answers with `[TokenTree]`, and writing one out by hand
//! -- `Group Paren [Word "x" Nowhere, …] Nowhere` -- buries what it says. A
//! quote is the tokens themselves, with `$name` or `$(an expression)` where
//! something goes in:
//!
//! ```text
//! quote! { fun $name x = $(body x) }
//! ```
//!
//! It is expanded where it is written, in the macro's own package, into an
//! expression that builds the tokens: each one it writes a `TokenTree` standing
//! `Nowhere` -- where the macro's call will be -- and each hole whatever
//! `toTokens` makes of what is spliced, which for tokens the macro was given is
//! those tokens, still where the caller wrote them. `$$` is a `$`.
//!
//! What it writes names `quoted`, `toTokens` and the types `TokenTree`,
//! `Delim` and `Loc`, so they have to be in scope where the quote is:
//! `use Std.Macro (TokenTree, Delim, Loc, ToTokens, quoted)`.

use meadow_lexer::{Token, tt};
use meadow_span::Span;

/// The expression that builds what `trees` quote, or what is wrong and where.
pub fn expression(trees: &[tt::TokenTree]) -> Result<String, (String, Span)> {
    Ok(format!("quoted #[{}]", parts(trees)?))
}

/// The runs of tokens and the splices between them, as the elements of the
/// array `quoted` joins.
fn parts(trees: &[tt::TokenTree]) -> Result<String, (String, Span)> {
    let mut out: Vec<String> = Vec::new();
    let mut run: Vec<String> = Vec::new();
    let flush = |run: &mut Vec<String>, out: &mut Vec<String>| {
        if !run.is_empty() {
            out.push(format!("[{}]", run.join(", ")));
            run.clear();
        }
    };
    let mut i = 0;
    while i < trees.len() {
        let dollar = matches!(&trees[i], tt::TokenTree::Token(t) if *t.value() == Token::Dollar);
        if dollar {
            match trees.get(i + 1) {
                // `$$`: a `$` of its own.
                Some(tt::TokenTree::Token(t)) if *t.value() == Token::Dollar => {
                    run.push(token("Punct", &quoted_text("$")));
                    i += 2;
                    continue;
                }
                // `$name`: what `name` stands for.
                Some(tt::TokenTree::Token(t))
                    if matches!(t.value(), Token::LowerIdent(_) | Token::UpperIdent(_)) =>
                {
                    flush(&mut run, &mut out);
                    out.push(format!("toTokens {}", t.value().text()));
                    i += 2;
                    continue;
                }
                // `$(expr)`: what the expression makes.
                Some(tt::TokenTree::Group(g)) if g.delim == tt::Delim::Paren => {
                    flush(&mut run, &mut out);
                    out.push(format!("toTokens ({})", tt::render(&g.trees)));
                    i += 2;
                    continue;
                }
                _ => {
                    return Err((
                        "a `$` in a quote is followed by a name, `(an expression)`, or another `$`"
                            .to_string(),
                        trees[i].span(),
                    ));
                }
            }
        }
        run.push(tree(&trees[i])?);
        i += 1;
    }
    flush(&mut run, &mut out);
    Ok(out.join(", "))
}

/// One tree the quote writes as it is.
fn tree(t: &tt::TokenTree) -> Result<String, (String, Span)> {
    match t {
        tt::TokenTree::Group(g) => {
            let delim = match g.delim {
                tt::Delim::Paren => "Paren",
                tt::Delim::Brack => "Bracket",
                tt::Delim::Brace => "Brace",
            };
            Ok(format!(
                "TokenTree.Group Delim.{delim} (quoted #[{}]) Loc.Nowhere",
                parts(&g.trees)?
            ))
        }
        tt::TokenTree::Token(tok) => Ok(match tok.value() {
            Token::String(s) => token("Str", &quoted_text(s)),
            Token::Char(c) => token("Chr", &quoted_text(&c.to_string())),
            Token::Int(n) => token("Num", &format!("({n})")),
            Token::Real(bits) => token("Real", &format!("({:?})", f64::from_bits(*bits))),
            Token::InterpStart(_) | Token::InterpMid(_) | Token::InterpEnd(_) => {
                return Err((
                    "a quote cannot hold a string with holes in it; splice one: `$(\"…\")`"
                        .to_string(),
                    tok.span,
                ));
            }
            other => {
                let written = other.text();
                let word = written.starts_with(|c: char| c.is_alphabetic() || c == '_');
                token(if word { "Word" } else { "Punct" }, &quoted_text(&written))
            }
        }),
    }
}

fn token(ctor: &str, arg: &str) -> String {
    format!("TokenTree.{ctor} {arg} Loc.Nowhere")
}

/// `s` as a Meadow string literal.
fn quoted_text(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

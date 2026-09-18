//! Matching a call against a rule, and substituting into its template.
//!
//! A matcher is token trees with metavariables in them: `$name`, or
//! `$name : kind` to say what may stand there, and `$( … )sep*` for a run.
//! Matching binds each metavariable to the trees it stood for; substitution
//! puts them back into the template. Neither side parses anything -- what comes
//! out is tokens, and [`super`] hands those to the parser.
//!
//! ```text
//! macro vec
//!   | ()                 -> { empty }
//!   | ($x)               -> { push empty $x }
//!   | ($x, $( $rest ),+) -> { push (vec!($( $rest ),+)) $x }
//! ```

use meadow_intern::InternedString;
use meadow_lexer::{LToken, Token, tt};
use meadow_span::Span;
use std::collections::HashMap;

/// What may stand where a metavariable is written.
///
/// `tt`, `ident` and `lit` need no parser: they are a count of token trees and
/// a check on the token. `expr`, `pat` and `item` are read by the parser
/// itself, which is safe only because of the follow rules below: they say where
/// the fragment stops, so the parser is handed a run of tokens rather than
/// asked to stop on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fragment {
    /// One token tree, whatever it is.
    Tt,
    /// One identifier.
    Ident,
    /// One literal: a number, a string, or a character.
    Lit,
    /// An expression.
    Expr,
    /// A pattern.
    Pat,
    /// One declaration.
    Item,
}

impl Fragment {
    pub fn of(name: &str) -> Option<Self> {
        match name {
            "tt" => Some(Fragment::Tt),
            "ident" => Some(Fragment::Ident),
            "lit" => Some(Fragment::Lit),
            "expr" => Some(Fragment::Expr),
            "pat" => Some(Fragment::Pat),
            "item" => Some(Fragment::Item),
            _ => None,
        }
    }

    /// Whether this is read by the parser, and so runs until something says it
    /// has ended rather than taking one tree.
    fn parsed(self) -> bool {
        matches!(self, Fragment::Expr | Fragment::Pat | Fragment::Item)
    }

    /// Whether `t` is the sort of token this stands for. `Tt` takes any tree,
    /// so it never gets this far, and neither does anything the parser reads.
    fn accepts(self, t: &Token) -> bool {
        match self {
            Fragment::Ident => matches!(t, Token::LowerIdent(_) | Token::UpperIdent(_)),
            Fragment::Lit => matches!(
                t,
                Token::Int(_) | Token::Real(_) | Token::String(_) | Token::Char(_)
            ),
            _ => true,
        }
    }

    /// The word it is written with, for a message about it.
    fn name(self) -> &'static str {
        match self {
            Fragment::Tt => "tt",
            Fragment::Ident => "ident",
            Fragment::Lit => "lit",
            Fragment::Expr => "expr",
            Fragment::Pat => "pat",
            Fragment::Item => "item",
        }
    }
}

/// What may follow a fragment the parser reads.
///
/// Application in Meadow is juxtaposition, so an expression does not end where
/// a Rust one would: in `($f : expr $x : expr)` the first fragment would
/// swallow the second, and no care in the matcher changes that. So a fragment
/// may only be followed by a token that cannot be part of it, and a matcher
/// that puts anything else after one is refused where it is written rather than
/// where it is called.
const FOLLOW: &[Token] = &[
    Token::Comma,
    Token::SemiColon,
    Token::RArrow,
    Token::Bar,
    Token::Then,
    Token::Else,
    Token::In,
    Token::With,
];

/// How the follow rule reads in a message.
fn follows() -> String {
    let each: Vec<String> = FOLLOW.iter().map(|t| format!("`{}`", t.text())).collect();
    format!("{}, or a closing bracket", each.join(", "))
}

/// How many times a repetition may occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    /// `*`
    Any,
    /// `+`
    AtLeastOne,
    /// `?`
    AtMostOne,
}

/// One piece of a matcher, as it is read rather than as it is written.
#[derive(Debug, Clone)]
enum Piece {
    /// A token that must be there exactly.
    Exact(Token, Span),
    /// A bracketed run, whose contents are pieces of their own.
    Group(tt::Delim, Vec<Piece>, Span),
    /// `$name : kind`
    Var(InternedString, Fragment, Span),
    /// `$( … )sep rep`
    Repeat {
        inner: Vec<Piece>,
        sep: Option<Token>,
        rep: Repeat,
        span: Span,
    },
    /// `$$` -- a literal `$`.
    Dollar(Span),
}

/// What a metavariable stood for.
#[derive(Debug, Clone)]
pub enum Binding {
    /// Outside any repetition: the trees it matched.
    One(Vec<tt::TokenTree>),
    /// Inside one: what it matched on each pass, in order.
    Many(Vec<Binding>),
}

pub type Bindings = HashMap<InternedString, Binding>;

/// Something wrong with how a macro is written, as opposed to how it is called.
pub struct Invalid {
    pub msg: String,
    pub label: String,
    pub span: Span,
}

fn invalid(msg: impl Into<String>, label: impl Into<String>, span: Span) -> Invalid {
    Invalid {
        msg: msg.into(),
        label: label.into(),
        span,
    }
}

// --- reading a matcher --------------------------------------------------------

/// Read `trees` as a matcher, or say what is wrong with it.
///
/// This is checked once when the macro is defined rather than at every call, so
/// a matcher that could never work is reported where it was written.
fn pieces(trees: &[tt::TokenTree]) -> Result<Vec<Piece>, Invalid> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < trees.len() {
        match &trees[i] {
            tt::TokenTree::Group(g) => {
                out.push(Piece::Group(g.delim, pieces(&g.trees)?, g.span()));
                i += 1;
            }
            tt::TokenTree::Token(t) if *t.value() == Token::Dollar => {
                let (piece, used) = dollar(trees, i, t.span)?;
                out.push(piece);
                i += used;
            }
            tt::TokenTree::Token(t) => {
                out.push(Piece::Exact(t.value().clone(), t.span));
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Read what follows the `$` at `i`, and say how many trees it took.
fn dollar(trees: &[tt::TokenTree], i: usize, at: Span) -> Result<(Piece, usize), Invalid> {
    match trees.get(i + 1) {
        // `$$` -- an escaped dollar.
        Some(tt::TokenTree::Token(t)) if *t.value() == Token::Dollar => {
            Ok((Piece::Dollar(at.extend(t.span)), 2))
        }
        // `$name` or `$name : kind`
        Some(tt::TokenTree::Token(t)) if let Some(name) = ident_of(t.value()) => {
            let span = at.extend(t.span);
            let colon = matches!(
                trees.get(i + 2),
                Some(tt::TokenTree::Token(c)) if *c.value() == Token::Colon
            );
            if !colon {
                // A bare `$x` is a token tree: the commonest fragment, and the
                // one that needs no parser.
                return Ok((Piece::Var(name, Fragment::Tt, span), 2));
            }
            let Some(tt::TokenTree::Token(k)) = trees.get(i + 3) else {
                return Err(invalid(
                    format!("`${name} :` does not say what may stand there"),
                    "a fragment kind belongs here",
                    span,
                ));
            };
            let Some(kind) = ident_of(k.value()).and_then(|n| Fragment::of(&n)) else {
                return Err(invalid(
                    format!("`{}` is not a fragment kind", k.value().text()),
                    "the kinds are `tt`, `ident`, `lit`, `expr`, `pat` and `item`",
                    k.span,
                ));
            };
            Ok((Piece::Var(name, kind, span.extend(k.span)), 4))
        }
        // `$( … )sep rep`
        Some(tt::TokenTree::Group(g)) if g.delim == tt::Delim::Paren => {
            let inner = pieces(&g.trees)?;
            // What follows is an optional separator and then the count.
            let (sep, rep, end) = repeat_of(trees, i + 2, at.extend(g.span()))?;
            let used = 2 + if sep.is_some() { 2 } else { 1 };
            Ok((
                Piece::Repeat {
                    inner,
                    sep,
                    rep,
                    span: at.extend(end),
                },
                used,
            ))
        }
        _ => Err(invalid(
            "a `$` on its own",
            "expected a name, a `(`, or another `$`",
            at,
        )),
    }
}

/// The separator and count after a `$( … )`, and where they end.
fn repeat_of(
    trees: &[tt::TokenTree],
    i: usize,
    at: Span,
) -> Result<(Option<Token>, Repeat, Span), Invalid> {
    let token = |k: usize| match trees.get(k) {
        Some(tt::TokenTree::Token(t)) => Some((t.value().clone(), t.span)),
        _ => None,
    };
    let count = |t: &Token| match t {
        Token::Star => Some(Repeat::Any),
        Token::Plus => Some(Repeat::AtLeastOne),
        // `?` is not punctuation of its own: it lexes as an operator name.
        Token::OpIdent(s) if &**s == "?" => Some(Repeat::AtMostOne),
        _ => None,
    };
    match token(i) {
        None => Err(invalid(
            "a repetition that does not say how many",
            "expected `*`, `+` or `?` after this",
            at,
        )),
        Some((t, span)) => match count(&t) {
            // No separator: `$( … )*`
            Some(rep) => Ok((None, rep, span)),
            // A separator, then the count: `$( … ),*`
            None => match token(i + 1) {
                Some((next, span)) if let Some(rep) = count(&next) => Ok((Some(t), rep, span)),
                _ => Err(invalid(
                    "a repetition that does not say how many",
                    format!("expected `*`, `+` or `?` after this `{}`", t.text()),
                    span,
                )),
            },
        },
    }
}

/// The name of an identifier token, if it is one.
/// A package's name as a token: which identifier it is follows its case, as
/// everything in Meadow does.
fn package_token(pkg: InternedString) -> Token {
    if pkg.to_string().starts_with(|c: char| c.is_uppercase()) {
        Token::UpperIdent(pkg)
    } else {
        Token::LowerIdent(pkg)
    }
}

fn ident_of(t: &Token) -> Option<InternedString> {
    match t {
        Token::LowerIdent(s) | Token::UpperIdent(s) => Some(*s),
        _ => None,
    }
}

/// Every metavariable a run of pieces binds.
fn bound_by(pieces: &[Piece], out: &mut Vec<InternedString>) {
    for p in pieces {
        match p {
            Piece::Var(name, _, _) => out.push(*name),
            Piece::Group(_, inner, _) => bound_by(inner, out),
            Piece::Repeat { inner, .. } => bound_by(inner, out),
            Piece::Exact(_, _) | Piece::Dollar(_) => {}
        }
    }
}

// --- matching -----------------------------------------------------------------

/// A matcher, read and checked once.
pub struct Matcher {
    pieces: Vec<Piece>,
}

impl Matcher {
    /// Read `trees` as a matcher.
    pub fn read(trees: &[tt::TokenTree]) -> Result<Self, Invalid> {
        let pieces = pieces(trees)?;
        // Where every parsed fragment ends has to be clear from the matcher
        // alone, and that is decided here rather than at a call.
        check_follow(&pieces, Ends::Bracket)?;
        // A name bound twice would make substitution ambiguous.
        let mut names = Vec::new();
        bound_by(&pieces, &mut names);
        if let Some(n) = names.iter().find(|n| &***n == "pkg") {
            return Err(invalid(
                format!("`${n}` is the package a macro was written in"),
                "this name is taken",
                span_of(&pieces).unwrap_or_default(),
            ));
        }
        for (i, n) in names.iter().enumerate() {
            if names[..i].contains(n) {
                return Err(invalid(
                    format!("`${n}` is bound twice in one rule"),
                    "each name may stand for one thing",
                    span_of(&pieces).unwrap_or_default(),
                ));
            }
        }
        Ok(Matcher { pieces })
    }

    /// Match `arg` against this matcher, binding what it stood for. `None` when
    /// it does not match -- which is not an error: the next rule is tried.
    pub fn match_trees(&self, arg: &[tt::TokenTree]) -> Option<Bindings> {
        let mut out = Bindings::new();
        match_pieces(&self.pieces, arg, &mut out).then_some(out)
    }
}

/// What comes after a run of pieces, for a fragment written at the end of it.
#[derive(Clone, Copy)]
enum Ends<'a> {
    /// A closing bracket, which ends anything.
    Bracket,
    /// A repetition's separator.
    Separator(&'a Token),
    /// Nothing that could end a fragment.
    Nothing,
}

/// Check that every fragment the parser reads is followed by something that
/// says where it ends.
fn check_follow(pieces: &[Piece], after: Ends<'_>) -> Result<(), Invalid> {
    for (i, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Group(_, inner, _) => check_follow(inner, Ends::Bracket)?,
            Piece::Repeat { inner, sep, .. } => {
                // Every pass of a repetition but the last ends at its
                // separator, and the last ends at whatever follows the
                // repetition -- so a fragment written at the end of one needs
                // both of those to be able to end it.
                let between = match sep {
                    Some(t) if FOLLOW.contains(t) => Ends::Separator(t),
                    _ => Ends::Nothing,
                };
                let ends = match (between, ended_after(pieces, i, after)) {
                    (Ends::Separator(t), true) => Ends::Separator(t),
                    _ => Ends::Nothing,
                };
                check_follow(inner, ends)?;
            }
            Piece::Var(name, kind, span) if kind.parsed() && !ended_after(pieces, i, after) => {
                return Err(invalid(
                    format!("nothing says where `${name} : {}` ends", kind.name()),
                    format!("follow it with {}", follows()),
                    *span,
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether what comes after the piece at `i` can end a fragment.
fn ended_after(pieces: &[Piece], i: usize, after: Ends<'_>) -> bool {
    match pieces.get(i + 1) {
        None => match after {
            Ends::Bracket => true,
            Ends::Separator(t) => FOLLOW.contains(t),
            Ends::Nothing => false,
        },
        Some(Piece::Exact(t, _)) => FOLLOW.contains(t),
        // A group, another fragment or a repetition: whatever that matched
        // could have been part of this one instead.
        Some(_) => false,
    }
}

fn span_of(pieces: &[Piece]) -> Option<Span> {
    let one = |p: &Piece| match p {
        Piece::Exact(_, s)
        | Piece::Group(_, _, s)
        | Piece::Var(_, _, s)
        | Piece::Repeat { span: s, .. }
        | Piece::Dollar(s) => *s,
    };
    let first = pieces.first().map(one)?;
    Some(first.extend(pieces.last().map(one).unwrap_or(first)))
}

/// Match `pieces` against exactly the whole of `trees`.
fn match_pieces(pieces: &[Piece], trees: &[tt::TokenTree], out: &mut Bindings) -> bool {
    let mut at = 0;
    for (i, piece) in pieces.iter().enumerate() {
        match piece {
            Piece::Exact(want, _) => {
                let Some(tt::TokenTree::Token(t)) = trees.get(at) else {
                    return false;
                };
                if t.value() != want {
                    return false;
                }
                at += 1;
            }
            Piece::Dollar(_) => {
                let Some(tt::TokenTree::Token(t)) = trees.get(at) else {
                    return false;
                };
                if *t.value() != Token::Dollar {
                    return false;
                }
                at += 1;
            }
            Piece::Group(delim, inner, _) => {
                let Some(tt::TokenTree::Group(g)) = trees.get(at) else {
                    return false;
                };
                if g.delim != *delim || !match_pieces(inner, &g.trees, out) {
                    return false;
                }
                at += 1;
            }
            Piece::Var(name, kind, _) if kind.parsed() => {
                // The fragment runs to the token the matcher says follows it --
                // which the follow rules guarantee cannot be part of it -- or
                // to the end of what is being matched.
                let end = match follow_token(pieces, i) {
                    Some(t) => match trees[at..].iter().position(|tr| is_token(tr, t)) {
                        Some(n) => at + n,
                        None => return false,
                    },
                    None => trees.len(),
                };
                let Some(bound) = fragment(*kind, &trees[at..end]) else {
                    // Not that sort of fragment after all, so this rule does not
                    // match and the next is tried.
                    return false;
                };
                out.insert(*name, Binding::One(bound));
                at = end;
            }
            Piece::Var(name, kind, _) => {
                let Some(tree) = trees.get(at) else {
                    return false;
                };
                if *kind != Fragment::Tt {
                    let tt::TokenTree::Token(t) = tree else {
                        return false;
                    };
                    if !kind.accepts(t.value()) {
                        return false;
                    }
                }
                out.insert(*name, Binding::One(vec![tree.clone()]));
                at += 1;
            }
            Piece::Repeat {
                inner, sep, rep, ..
            } => {
                // Greedy, and then the rest of the matcher must fit what is
                // left. A repetition is nearly always last or followed by a
                // token it cannot match, so there is nothing to back out of.
                let tail = &pieces[i + 1..];
                let (passes, used) = match_repeat(inner, sep.as_ref(), &trees[at..], tail);
                let enough = match rep {
                    Repeat::Any => true,
                    Repeat::AtLeastOne => !passes.is_empty(),
                    Repeat::AtMostOne => passes.len() <= 1,
                };
                if !enough {
                    return false;
                }
                let mut names = Vec::new();
                bound_by(inner, &mut names);
                for n in names {
                    let each = passes
                        .iter()
                        .map(|p| p.get(&n).cloned().unwrap_or(Binding::One(Vec::new())))
                        .collect();
                    out.insert(n, Binding::Many(each));
                }
                at += used;
                // The tail was matched as part of deciding where to stop.
                return match_pieces(tail, &trees[at..], out);
            }
        }
    }
    at == trees.len()
}

/// The token that ends the fragment at `i`, or `None` when what ends it is the
/// end of the run. Which of the two it is was settled by [`check_follow`] when
/// the macro was defined.
fn follow_token(pieces: &[Piece], i: usize) -> Option<&Token> {
    match pieces.get(i + 1) {
        Some(Piece::Exact(t, _)) => Some(t),
        _ => None,
    }
}

fn is_token(tree: &tt::TokenTree, want: &Token) -> bool {
    matches!(tree, tt::TokenTree::Token(t) if t.value() == want)
}

/// `trees` as a fragment of this kind, as it will be written back.
///
/// The parser is what decides: a run that does not parse is not that kind of
/// fragment, and the rule simply does not match. An expression and a pattern
/// come back **parenthesised**, because a fragment has to stay one thing when
/// it is written into a template -- `$x` bound to `a + b` under `f $x` is
/// `f (a + b)`, which is what was passed, and not `(f a) + b`. Rust uses an
/// invisible bracket for this; a real one costs nothing here and is visible in
/// `stringify!`, where showing it is no worse than hiding it.
fn fragment(kind: Fragment, trees: &[tt::TokenTree]) -> Option<Vec<tt::TokenTree>> {
    if trees.is_empty() {
        return None;
    }
    let tokens = tt::flatten(trees);
    let eoi = trees[0].span().extend(trees[trees.len() - 1].span());
    let parses = match kind {
        Fragment::Expr => {
            let (out, errs) = meadow_parser::parse_expr(&tokens, eoi);
            out.is_some() && errs.is_empty()
        }
        Fragment::Pat => {
            let (out, errs) = meadow_parser::parse_pat(&tokens, eoi);
            out.is_some() && errs.is_empty()
        }
        Fragment::Item => {
            let (out, errs) = meadow_parser::parse_decls(&tokens, eoi);
            errs.is_empty() && out.is_some_and(|ds| ds.len() == 1)
        }
        _ => true,
    };
    if !parses {
        return None;
    }
    Some(match kind {
        // A declaration is never a part of something larger, so nothing has to
        // hold it together.
        Fragment::Item => trees.to_vec(),
        _ => vec![tt::TokenTree::Group(tt::Group {
            delim: tt::Delim::Paren,
            trees: trees.to_vec(),
            // The brackets are not in the source, so they stand where the
            // fragment does: an error inside one still points at what was
            // written.
            open: Span::new(eoi.start, eoi.start),
            close: Span::new(eoi.end, eoi.end),
        })],
    })
}

/// Match `inner` as many times as it fits, separated by `sep`, leaving enough
/// for `tail`. Answers what each pass bound and how many trees were used.
///
/// Greedy, but never into what the rest of the matcher needs: a repetition
/// followed by `; $b` stops two trees from the end, so the `;` and the `$b` are
/// still there for it. That is what makes `($( $a ),* ; $b)` work, and it is
/// why the tail is passed in rather than matched afterwards.
///
/// The bound is exact only when the tail takes a fixed number of trees, which
/// is every tail without a repetition of its own. Two repetitions in one
/// matcher would need real backtracking, and are not supported.
fn match_repeat(
    inner: &[Piece],
    sep: Option<&Token>,
    trees: &[tt::TokenTree],
    tail: &[Piece],
) -> (Vec<Bindings>, usize) {
    let keep = fixed_len(tail).unwrap_or(0);
    let limit = trees.len().saturating_sub(keep);
    let mut passes = Vec::new();
    let mut at = 0;
    while at < limit {
        // Between passes, the separator.
        let start = if passes.is_empty() {
            at
        } else {
            match (sep, trees.get(at)) {
                (Some(want), Some(tt::TokenTree::Token(t))) if t.value() == want => at + 1,
                (Some(_), _) => break,
                (None, _) => at,
            }
        };
        if start > limit {
            break;
        }
        // How far this pass reaches: to the next separator, or to as many trees
        // as the pieces ask for when there is none.
        let end = match sep {
            Some(want) => next_at(&trees[start..limit], want).map_or(limit, |k| start + k),
            None => (start + fixed_len(inner).unwrap_or(1)).min(limit),
        };
        if start > end {
            break;
        }
        let mut bound = Bindings::new();
        if !match_pieces(inner, &trees[start..end], &mut bound) {
            break;
        }
        passes.push(bound);
        at = end;
    }
    (passes, at)
}

/// The index of the next top-level `want` in `trees`.
fn next_at(trees: &[tt::TokenTree], want: &Token) -> Option<usize> {
    trees
        .iter()
        .position(|t| matches!(t, tt::TokenTree::Token(t) if t.value() == want))
}

/// How many trees these pieces take, when that does not depend on the input.
fn fixed_len(pieces: &[Piece]) -> Option<usize> {
    let mut n = 0;
    for p in pieces {
        match p {
            Piece::Exact(_, _) | Piece::Group(_, _, _) | Piece::Var(_, _, _) | Piece::Dollar(_) => {
                n += 1
            }
            Piece::Repeat { .. } => return None,
        }
    }
    Some(n)
}

// --- substituting -------------------------------------------------------------

/// Put `bound` into `template`, answering the tokens it produced.
///
/// `at` is the call's span, which every token the template wrote is given:
/// the template is not in the file being compiled, so the call is the nearest
/// thing an error can point at. Tokens that came from the call keep their own.
pub fn substitute(
    template: &[tt::TokenTree],
    bound: &Bindings,
    at: Span,
    pkg: InternedString,
    mark: &dyn Fn(&Token) -> Token,
) -> Result<Vec<LToken>, Invalid> {
    let mut out = Vec::new();
    write(template, bound, at, pkg, mark, &mut out)?;
    Ok(out)
}

fn write(
    template: &[tt::TokenTree],
    bound: &Bindings,
    at: Span,
    pkg: InternedString,
    mark: &dyn Fn(&Token) -> Token,
    out: &mut Vec<LToken>,
) -> Result<(), Invalid> {
    let mut i = 0;
    while i < template.len() {
        match &template[i] {
            tt::TokenTree::Group(g) => {
                out.push(LToken::new(g.delim.open(), at));
                write(&g.trees, bound, at, pkg, mark, out)?;
                out.push(LToken::new(g.delim.close(), at));
                i += 1;
            }
            tt::TokenTree::Token(t) if *t.value() == Token::Dollar => {
                i += splice(template, i, bound, at, pkg, mark, out, t.span)?;
            }
            tt::TokenTree::Token(t) => {
                out.push(LToken::new(mark(t.value()), at));
                i += 1;
            }
        }
    }
    Ok(())
}

/// Write what the `$` at `i` stands for, and say how many trees it took.
fn splice(
    template: &[tt::TokenTree],
    i: usize,
    bound: &Bindings,
    at: Span,
    pkg: InternedString,
    mark: &dyn Fn(&Token) -> Token,
    out: &mut Vec<LToken>,
    span: Span,
) -> Result<usize, Invalid> {
    match template.get(i + 1) {
        // `$$` is one `$`.
        Some(tt::TokenTree::Token(t)) if *t.value() == Token::Dollar => {
            out.push(LToken::new(Token::Dollar, at));
            let _ = t;
            Ok(2)
        }
        // `$pkg` -- the package the macro was written in, so that what a
        // template names resolves where the macro is, not where it lands.
        Some(tt::TokenTree::Token(t)) if ident_of(t.value()).is_some_and(|n| &*n == "pkg") => {
            out.push(LToken::new(package_token(pkg), at));
            let _ = t;
            Ok(2)
        }
        // `$name`
        Some(tt::TokenTree::Token(t)) if let Some(name) = ident_of(t.value()) => {
            match bound.get(&name) {
                Some(Binding::One(trees)) => {
                    // Straight from the call, so it keeps its own spans: an
                    // error in an argument is reported where it was written.
                    out.extend(tt::flatten(trees));
                    Ok(2)
                }
                Some(Binding::Many(_)) => Err(invalid(
                    format!("`${name}` stands for a run of things"),
                    "it needs a `$( … )` around it to say how they are written",
                    span.extend(t.span),
                )),
                None => Err(invalid(
                    format!("`${name}` is not bound by this rule's matcher"),
                    "no such name is matched",
                    span.extend(t.span),
                )),
            }
        }
        // `$( … )sep rep`
        Some(tt::TokenTree::Group(g)) if g.delim == tt::Delim::Paren => {
            let (sep, _, _) = repeat_of(template, i + 2, span.extend(g.span()))?;
            let used = 2 + if sep.is_some() { 2 } else { 1 };
            // Which of the names inside vary, and how many passes they agree on.
            let mut names = Vec::new();
            dollar_names(&g.trees, &mut names);
            let counts: Vec<usize> = names
                .iter()
                .filter_map(|n| match bound.get(n) {
                    Some(Binding::Many(v)) => Some(v.len()),
                    _ => None,
                })
                .collect();
            let Some(&passes) = counts.first() else {
                return Err(invalid(
                    "a repetition with nothing to repeat",
                    "no name in here stands for a run of things",
                    span.extend(g.span()),
                ));
            };
            if counts.iter().any(|&c| c != passes) {
                return Err(invalid(
                    "the names in this repetition stand for different numbers of things",
                    "they have to be written the same number of times",
                    span.extend(g.span()),
                ));
            }
            for k in 0..passes {
                if k > 0
                    && let Some(s) = &sep
                {
                    out.push(LToken::new(s.clone(), at));
                }
                // Each pass sees the k-th of every run, and everything else as
                // it was.
                let mut pass = bound.clone();
                for n in &names {
                    if let Some(Binding::Many(v)) = bound.get(n) {
                        pass.insert(*n, v[k].clone());
                    }
                }
                write(&g.trees, &pass, at, pkg, mark, out)?;
            }
            Ok(used)
        }
        _ => Err(invalid(
            "a `$` on its own",
            "expected a name, a `(`, or another `$`",
            span,
        )),
    }
}

/// Every `$name` written in `trees`, at any depth.
fn dollar_names(trees: &[tt::TokenTree], out: &mut Vec<InternedString>) {
    let mut i = 0;
    while i < trees.len() {
        match &trees[i] {
            tt::TokenTree::Group(g) => {
                dollar_names(&g.trees, out);
                i += 1;
            }
            tt::TokenTree::Token(t) if *t.value() == Token::Dollar => {
                match trees.get(i + 1) {
                    Some(tt::TokenTree::Token(n)) if let Some(name) = ident_of(n.value()) => {
                        if !out.contains(&name) {
                            out.push(name);
                        }
                        i += 2;
                    }
                    // `$$`, or a nested `$( … )` whose names belong to it.
                    _ => i += 2,
                }
            }
            _ => i += 1,
        }
    }
}

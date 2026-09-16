//! Parsing, via `chumsky`.
//!
//! [`parse`] parses a whole module; [`parse_repl`] parses a single decl *or*
//! expression for the REPL. Expressions are built bottom-up: `atom` → `app`
//! (juxtaposition) → `pratt` (operators, with precedence/associativity given by
//! the `infix`/`prefix` levels). Type expressions have their own small grammar
//! ([`ty`] / [`ty_atom`]) used only inside `data` / `record` declarations.
//!
//! Errors are returned alongside a partial tree (`(Option<_>, Vec<Rich>)`); the
//! driver converts each `Rich` into a [`Diagnostic`].
//!
//! [`Diagnostic`]: meadow_diagnostics::Diagnostic

use chumsky::{
    IterParser, Parser,
    error::Rich,
    extra,
    input::{Input, ValueInput},
    pratt::{infix, left, none, prefix, right},
    primitive::*,
    recursive::recursive,
    select,
};
use itertools::Either;
use meadow_ast::*;
use meadow_intern::InternedString;
use meadow_lexer::{LToken, Token, tt};
use meadow_source::Source;
use meadow_span::{Located, Span};

/// Parse a whole module.
pub fn parse<'src>(
    name: InternedString,
    src: Source,
    tokens: &'src [LToken],
) -> (Option<LModule>, Vec<Rich<'src, Token, Span>>) {
    let stream = tokens.split_spanned(Span::from(0..src.len()));
    module(name).parse(stream).into_output_errors()
}

/// Parse a single REPL entry: either one declaration or one expression.
pub fn parse_repl<'src>(
    src: Source,
    tokens: &'src [LToken],
) -> (Option<Either<LDecl, LExpr>>, Vec<Rich<'src, Token, Span>>) {
    let stream = tokens.split_spanned(Span::from(0..src.len()));
    let p = choice((decl().map(Either::Left), expr().map(Either::Right)));
    p.parse(stream).into_output_errors()
}

/// Parse one expression, and nothing else.
///
/// `eoi` is where errors point when the tokens run out early. The three
/// `parse_*` entry points below exist for macro expansion, which parses what a
/// template produced with the entry point for the position the call was in (see
/// `docs/MACROS.md`); the tokens are then not a source file, so there is no
/// [`Source`] to take that span from.
pub fn parse_expr<'src>(
    tokens: &'src [LToken],
    eoi: Span,
) -> (Option<LExpr>, Vec<Rich<'src, Token, Span>>) {
    expr().parse(tokens.split_spanned(eoi)).into_output_errors()
}

/// Parse one pattern, and nothing else. See [`parse_expr`].
pub fn parse_pat<'src>(
    tokens: &'src [LToken],
    eoi: Span,
) -> (Option<LPat>, Vec<Rich<'src, Token, Span>>) {
    pat().parse(tokens.split_spanned(eoi)).into_output_errors()
}

/// Parse a run of declarations -- what a module body is, without the module.
/// Unlike [`parse`] this accepts none, since a macro may expand to nothing. See
/// [`parse_expr`].
pub fn parse_decls<'src>(
    tokens: &'src [LToken],
    eoi: Span,
) -> (Option<Vec<LDecl>>, Vec<Rich<'src, Token, Span>>) {
    decl()
        .repeated()
        .collect::<Vec<_>>()
        .parse(tokens.split_spanned(eoi))
        .into_output_errors()
}

fn module<'tokens, I>(
    name: InternedString,
) -> impl Parser<'tokens, I, LModule, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    decl()
        .repeated()
        .at_least(1)
        .collect()
        .map_with(move |decls, e| Located::new(Module { name, decls }, e.span()))
}

/// `@pub`, `@attr(A, B, C)`, `@cfg(all(unix, os = "linux"))` — a `@`, a name,
/// and an optional parenthesised list of arguments (see [`meta`]).
fn attr<'tokens, I>()
-> impl Parser<'tokens, I, Attr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    just(Token::At)
        .ignore_then(path_seg())
        .then(
            meta()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LParen), just(Token::RParen))
                .or_not(),
        )
        .map(|(name, meta)| {
            let meta = meta.unwrap_or_default();
            let args = meta
                .iter()
                .filter_map(|m| match m {
                    Meta::Word(n) => Some(n.clone()),
                    _ => None,
                })
                .collect();
            Attr { name, args, meta }
        })
}

/// An attribute argument: `name`, `name = "text"`, or `name(arg, …)`.
fn meta<'tokens, I>()
-> impl Parser<'tokens, I, Meta, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|meta| {
        let value = just(Token::Eq)
            .ignore_then(select! { Token::String(s) => s })
            .map_with(|s, e| Either::Left(Ident::new(s, e.span())));
        let list = meta
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map(Either::Right);
        path_seg()
            .then(choice((value, list)).or_not())
            .map(|(name, rest)| match rest {
                None => Meta::Word(name),
                Some(Either::Left(v)) => Meta::Value(name, v),
                Some(Either::Right(args)) => Meta::List(name, args),
            })
    })
}

fn decl<'tokens, I>()
-> impl Parser<'tokens, I, LDecl, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    attr()
        .repeated()
        .collect::<Vec<_>>()
        .then(bare_decl())
        .map_with(|(attrs, d), e| {
            if attrs.is_empty() {
                d
            } else {
                LDecl::new(Decl::Attributed(attrs, Box::new(d)), e.span())
            }
        })
}

fn bare_decl<'tokens, I>()
-> impl Parser<'tokens, I, LDecl, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    let bind_decl = {
        // `def x : Int = 5` — the same `: T` a `fun` writes before its `=`,
        // where with no parameters it describes the bound value itself.
        let pat_bind = just(Token::Def)
            .ignore_then(pat())
            .then(result_ty())
            .then_ignore(just(Token::Eq))
            .then(expr())
            .map(|((p, ret), e)| match ret {
                Some(t) => {
                    let span = p.span;
                    Bind::Pat(Located::new(Pat::Ann(Box::new(p), t), span), e)
                }
                None => Bind::Pat(p, e),
            });

        // `fun f a (x, y) = e`, or point-free `fun f = e` (no parameters) — the
        // latter is just a value binding, so it takes the `Bind::Pat` path (and
        // its right-hand side is subject to the value restriction, like `def`).
        // `| gcd a b = …`: another equation for the same function. The name is
        // written again, as it is in Haskell, so that a typo in it is caught
        // rather than quietly defining something else.
        let clause = just(Token::Bar)
            .ignore_then(value_ident())
            .then(param_pat().repeated().collect::<Vec<_>>())
            .then_ignore(just(Token::Eq))
            .then(expr())
            .map(|((name, args), body)| Clause { name, args, body });

        let fun_bind = just(Token::Fun)
            .ignore_then(value_ident())
            .then(param_pat().repeated().collect::<Vec<_>>())
            .then(result_ty())
            .then_ignore(just(Token::Eq))
            .then(expr())
            .then(clause.repeated().collect::<Vec<_>>())
            .validate(|((((name, args), ret), body), rest), e, emitter| {
                equations(name, args, ret, body, rest, e.span(), emitter)
            });

        fun_bind.or(pat_bind)
    };

    let mod_decl = just(Token::Mod)
        .ignore_then(path_seg())
        .map_with(|name, e| LDecl::new(Decl::Mod(name), e.span()));

    let use_decl = just(Token::Use)
        .ignore_then(
            path_seg()
                .separated_by(just(Token::Period))
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        // `.*` — every constructor of the type the path ends in. The lexer
        // reads `.*` as one operator token; `. *` spaced out is the same thing.
        .then(
            select! { Token::OpIdent(s) if &*s == ".*" => () }
                .or(just(Token::Period).then(just(Token::Star)).ignored())
                .or_not()
                .map(|g| g.is_some()),
        )
        // `as C` — rename the qualifier. Upper-case, because that is what a
        // qualified reference (`C.map`) can name.
        .then(just(Token::As).ignore_then(upper_ident()).or_not())
        // An operator may be named bare in the list, `(concat, ++)`, or in
        // its own parentheses, `((++))`, as it is written everywhere else.
        .then(
            path_seg()
                .or(user_op())
                .or(value_ident())
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LParen), just(Token::RParen))
                .or_not(),
        )
        .map_with(|(((path, glob), alias), names), e| {
            LDecl::new(
                Decl::Use(UseDecl {
                    path,
                    names: names.unwrap_or_default(),
                    glob,
                    alias,
                }),
                e.span(),
            )
        });

    let variant = upper_ident()
        .then(choice((
            field_list().map(VariantFields::Named),
            ty_atom()
                .repeated()
                .collect::<Vec<_>>()
                .map(VariantFields::Positional),
        )))
        .map(|(name, fields)| Variant { name, fields });

    let data_decl = just(Token::Data)
        .ignore_then(upper_ident())
        .then(lower_ident().repeated().collect::<Vec<_>>())
        .then_ignore(just(Token::Eq))
        .then(
            just(Token::Bar).or_not().ignore_then(
                variant
                    .separated_by(just(Token::Bar))
                    .at_least(1)
                    .collect::<Vec<_>>(),
            ),
        )
        .map_with(|((name, params), variants), e| {
            LDecl::new(
                Decl::Data(DataDecl {
                    name,
                    params,
                    variants,
                }),
                e.span(),
            )
        });

    let record_decl = just(Token::Record)
        .ignore_then(upper_ident())
        .then(lower_ident().repeated().collect::<Vec<_>>())
        .then_ignore(just(Token::Eq))
        .then(field_list())
        .map_with(|((name, params), fields), e| {
            LDecl::new(
                Decl::Record(RecordDecl {
                    name,
                    params,
                    fields,
                }),
                e.span(),
            )
        });

    let effect_decl = just(Token::Effect)
        .ignore_then(upper_ident())
        .then(lower_ident().repeated().collect::<Vec<_>>())
        .then(field_list())
        .map_with(|((name, params), ops), e| {
            LDecl::new(Decl::Effect(EffectDecl { name, params, ops }), e.span())
        });

    let type_decl = just(Token::Type)
        .ignore_then(upper_ident())
        .then(lower_ident().repeated().collect::<Vec<_>>())
        .then_ignore(just(Token::Eq))
        .then(ty())
        .map_with(|((name, params), ty), e| {
            LDecl::new(
                Decl::TypeAlias(TypeAliasDecl { name, params, ty }),
                e.span(),
            )
        });

    // `fun f : T` / `def x : T` -- a binding's type with no `=` after it: the
    // definition is elsewhere. Tried after a binding, which is what the same
    // start with an `=` is.
    let sig_decl = just(Token::Fun)
        .or(just(Token::Def))
        .ignore_then(value_ident())
        .then_ignore(just(Token::Colon))
        .then(ty())
        .map_with(|(name, ty), e| LDecl::new(Decl::Sig(name, ty), e.span()));

    choice((
        mod_decl,
        use_decl,
        data_decl,
        record_decl,
        effect_decl,
        type_decl,
        macro_decl().map_with(|m, e| LDecl::new(Decl::Macro(m), e.span())),
        // Before `sig_decl`, which also starts with a name: `derive! { … }`
        // would otherwise be read as the start of `derive : T`.
        mac_call().map_with(|m, e| LDecl::new(Decl::MacCall(m), e.span())),
        bind_decl.map_with(|bind, e| LDecl::new(Decl::Bind(bind), e.span())),
        sig_decl,
    ))
}

/// `{ @pub name : Type, age : Type, }` — shared by `record` decls, named variant
/// fields, and `effect` operation lists. Each field may carry leading attributes.
fn field_list<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Vec<Field>, extra::Err<Rich<'a, Token, Span>>> + Clone {
    attr()
        .repeated()
        .collect::<Vec<_>>()
        .then(lower_ident())
        .then_ignore(just(Token::Colon))
        .then(ty())
        .map(|((attrs, name), ty)| Field { attrs, name, ty })
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
}

/// A full type expression: `a`, `Vector a`, `Maybe (Vector Int)`, `(a, b)`, `[a]`,
/// `a -> b` (right-associative).
fn ty<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LType, extra::Err<Rich<'a, Token, Span>>> + Clone {
    recursive(|ty| {
        let unit = just(Token::LParen)
            .then(just(Token::RParen))
            .map_with(|_, e| {
                Located::new(
                    TypeExpr::Con(Ident::new(InternedString::from("Unit"), e.span()), vec![]),
                    e.span(),
                )
            });

        // `[a]` is a `Vector`; `[a;]` is a `List`. The `;` is the same marker the
        // `[x; y]` literal uses, so the type reads like the values it holds.
        let seq = ty
            .clone()
            .then(just(Token::SemiColon).or_not())
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|(t, semi), e| {
                let kind = match semi {
                    Some(_) => TypeExpr::List(t),
                    None => TypeExpr::Vector(t),
                };
                Located::new(kind, e.span())
            });

        let paren_or_tuple = ty
            .clone()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map_with(|mut ts, e| {
                if ts.len() == 1 {
                    ts.pop().unwrap()
                } else {
                    Located::new(TypeExpr::Tuple(ts), e.span())
                }
            });

        // `#[a]`, the builtin `Array` -- the way an array's type prints.
        let array = just(Token::Hash)
            .then(just(Token::LBrack))
            .ignore_then(ty.clone())
            .then_ignore(just(Token::RBrack))
            .map_with(|t, e| {
                Located::new(
                    TypeExpr::Con(Ident::new(InternedString::from("Array"), e.span()), vec![t]),
                    e.span(),
                )
            });

        let tvar = lower_ident().map_with(|n, e| Located::new(TypeExpr::Var(n), e.span()));
        let tcon0 = upper_ident().map_with(|n, e| Located::new(TypeExpr::Con(n, vec![]), e.span()));
        let record = record_ty(ty.clone());

        let atom = choice((unit, array, seq, paren_or_tuple, record, tvar, tcon0));

        let app = upper_ident()
            .then(atom.clone().repeated().at_least(1).collect::<Vec<_>>())
            .map_with(|(n, args), e| Located::new(TypeExpr::Con(n, args), e.span()));

        let head = choice((app, atom.clone()));

        // effect annotation after `!` — reuses the local `atom` for label args,
        // so it must live inside this `recursive` closure (no separate fn).
        let eff_label = upper_ident().then(atom.clone().repeated().collect::<Vec<_>>());
        let eff_braced = eff_label
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .then(just(Token::Bar).ignore_then(lower_ident()).or_not())
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map(|(labels, tail)| EffectRow { labels, tail });
        let eff_bare_var = lower_ident().map(|n| EffectRow {
            labels: vec![],
            tail: Some(n),
        });
        let eff_bare_label = eff_label.map(|l| EffectRow {
            labels: vec![l],
            tail: None,
        });
        let eff_row = choice((eff_braced, eff_bare_var, eff_bare_label));

        head.clone()
            .then(
                just(Token::RArrow)
                    .ignore_then(ty.clone())
                    .then(just(Token::Bang).ignore_then(eff_row).or_not())
                    .or_not(),
            )
            .map_with(|(l, r), e| match r {
                Some((rhs, eff)) => Located::new(TypeExpr::Fun(vec![l], rhs, eff), e.span()),
                None => l,
            })
    })
}

/// A single type *atom* — used for a `data` variant's positional fields, where
/// `Leaf (Vector a) Int` is two fields, not one application.
fn ty_atom<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LType, extra::Err<Rich<'a, Token, Span>>> + Clone {
    let inner = ty();
    let unit = just(Token::LParen)
        .then(just(Token::RParen))
        .map_with(|_, e| {
            Located::new(
                TypeExpr::Con(Ident::new(InternedString::from("Unit"), e.span()), vec![]),
                e.span(),
            )
        });
    let seq = inner
        .clone()
        .then(just(Token::SemiColon).or_not())
        .delimited_by(just(Token::LBrack), just(Token::RBrack))
        .map_with(|(t, semi), e| {
            let kind = match semi {
                Some(_) => TypeExpr::List(t),
                None => TypeExpr::Vector(t),
            };
            Located::new(kind, e.span())
        });
    let paren_or_tuple = inner
        .clone()
        .separated_by(just(Token::Comma))
        .at_least(1)
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LParen), just(Token::RParen))
        .map_with(|mut ts, e| {
            if ts.len() == 1 {
                ts.pop().unwrap()
            } else {
                Located::new(TypeExpr::Tuple(ts), e.span())
            }
        });
    // `#[a]`, the builtin `Array` -- the way an array's type prints.
    let array = just(Token::Hash)
        .then(just(Token::LBrack))
        .ignore_then(inner.clone())
        .then_ignore(just(Token::RBrack))
        .map_with(|t, e| {
            Located::new(
                TypeExpr::Con(Ident::new(InternedString::from("Array"), e.span()), vec![t]),
                e.span(),
            )
        });
    let tvar = lower_ident().map_with(|n, e| Located::new(TypeExpr::Var(n), e.span()));
    let tcon0 = upper_ident().map_with(|n, e| Located::new(TypeExpr::Con(n, vec![]), e.span()));
    // A variant's own `{ ... }` is its named fields, and `variant` tries that
    // first; a positional field of record type is written in parentheses.
    let record = record_ty(inner.clone());
    choice((unit, array, seq, paren_or_tuple, record, tvar, tcon0))
}

/// `{ name : String, age : Int }` or `{ name : String | r }` -- a structural
/// record type, spelled the way one is printed.
///
/// `{}` is the empty record, and `{ | r }` is any record at all. The tail is a
/// row variable, told apart from an ordinary one only by standing here.
fn record_ty<'a, I, P>(
    ty: P,
) -> impl Parser<'a, I, LType, extra::Err<Rich<'a, Token, Span>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = Span>,
    P: Parser<'a, I, LType, extra::Err<Rich<'a, Token, Span>>> + Clone,
{
    lower_ident()
        .then_ignore(just(Token::Colon))
        .then(ty)
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .then(just(Token::Bar).ignore_then(lower_ident()).or_not())
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map_with(|(fields, tail), e| Located::new(TypeExpr::Record(fields, tail), e.span()))
}

fn path_seg<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> + Clone {
    select! {
        Token::LowerIdent(name) => name,
        Token::UpperIdent(name) => name,
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

/// A bracketed run of token trees, and where its brackets were.
fn group<'a, I, P>(
    tree: P,
    delim: tt::Delim,
) -> impl Parser<'a, I, tt::Group, extra::Err<Rich<'a, Token, Span>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = Span>,
    P: Parser<'a, I, tt::TokenTree, extra::Err<Rich<'a, Token, Span>>> + Clone,
{
    just(delim.open())
        .map_with(|_, e| e.span())
        .then(tree.repeated().collect::<Vec<_>>())
        .then(just(delim.close()).map_with(|_, e| e.span()))
        .map(move |((open, trees), close)| tt::Group {
            delim,
            trees,
            open,
            close,
        })
}

/// One token tree: a token, or a bracketed run of them.
///
/// Balanced brackets are the only structure required of a macro's argument, so
/// this accepts any token that is not one — the argument need not be an
/// expression, or mean anything at all until the macro is expanded.
fn token_tree<'a, I>()
-> impl Parser<'a, I, tt::TokenTree, extra::Err<Rich<'a, Token, Span>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = Span>,
{
    recursive(|tree| {
        let bracketed = choice((
            group(tree.clone(), tt::Delim::Paren),
            group(tree.clone(), tt::Delim::Brack),
            group(tree, tt::Delim::Brace),
        ))
        .map(tt::TokenTree::Group);
        let single = any()
            .filter(|t: &Token| {
                tt::Delim::opened_by(t).is_none() && tt::Delim::closed_by(t).is_none()
            })
            .map_with(|t, e| tt::TokenTree::Token(LToken::new(t, e.span())));
        bracketed.or(single)
    })
}

/// A bracketed run of token trees in any of the three brackets: what both
/// sides of a macro rule are written with, and what a call's argument is.
fn any_group<'a, I>() -> impl Parser<'a, I, tt::Group, extra::Err<Rich<'a, Token, Span>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = Span>,
{
    choice((
        group(token_tree(), tt::Delim::Paren),
        group(token_tree(), tt::Delim::Brack),
        group(token_tree(), tt::Delim::Brace),
    ))
}

/// A macro definition:
///
/// ```text
/// macro swap
///   | ($a, $b) -> { ($b, $a) }
/// ```
///
/// Rules are written the way a `match`'s arms are, and are tried in the same
/// order. Both sides are token trees, so nothing here reads what they mean.
fn macro_decl<'a, I>() -> impl Parser<'a, I, MacroDef, extra::Err<Rich<'a, Token, Span>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = Span>,
{
    let rule = just(Token::Bar)
        .ignore_then(any_group())
        .then_ignore(just(Token::RArrow))
        .then(any_group())
        .map(|(matcher, template)| MacroRule { matcher, template });
    just(Token::Macro)
        .ignore_then(lower_ident())
        .then(rule.repeated().at_least(1).collect::<Vec<_>>())
        .map(|(name, rules)| MacroDef { name, rules })
}

/// A macro call: `assertEq!(got, want)`, `vec![1; 2]`, `config! { … }`, and
/// qualified, `Std.Test.assertEq!(…)`.
///
/// The three brackets mean the same thing; which one to use is a question of
/// how the call reads. Nothing here looks inside them.
fn mac_call<'a, I>() -> impl Parser<'a, I, MacCall, extra::Err<Rich<'a, Token, Span>>> + Clone
where
    I: ValueInput<'a, Token = Token, Span = Span>,
{
    path_seg()
        .separated_by(just(Token::Period))
        .at_least(1)
        .collect::<Vec<_>>()
        // `!=` is one token, so `foo != x` can never be mistaken for a call.
        .then_ignore(just(Token::Bang))
        .then(any_group())
        .map(|(path, arg)| MacCall { path, arg })
}

fn expr<'tokens, I>()
-> impl Parser<'tokens, I, LExpr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|expr| {
        let lit_expr = located(lit().map(Expr::Lit));
        let var_expr = located(value_ident().map(Expr::Var));
        let unit_expr = located(
            just(Token::LParen)
                .then(just(Token::RParen))
                .map(|_| Expr::Unit),
        );

        let tuple = expr
            .clone()
            .separated_by(just(Token::Comma))
            .at_least(2)
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map_with(|es: Vec<LExpr>, e| Located::new(Expr::Tuple(es), e.span()))
            .boxed();

        // `[lo .. hi]` / `[lo ..= hi]` — an inclusive integer range (Haskell-style),
        // desugared to `range lo (hi + 1)`.
        let range_list = expr
            .clone()
            .then(choice((
                just(Token::DoublePeriod),
                just(Token::DoublePeriodEq),
            )))
            .then(expr.clone())
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|((lo, _), hi), e| {
                let span = e.span();
                let one = Located::new(Expr::Lit(Lit::Int(1)), hi.span);
                let hi1 = Located::new(
                    Expr::BinOp(Located::new(BinOp::Add, hi.span), hi, one),
                    span,
                );
                let range = Located::new(
                    Expr::Var(Located::new(InternedString::from("range"), span)),
                    span,
                );
                Located::new(Expr::App(range, vec![lo, hi1]), span)
            })
            .boxed();

        // `#[e, ...]` — a builtin `Array` literal (`Token::Hash` then `Token::LBrack`).
        let array_expr = expr
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(
                just(Token::Hash).then(just(Token::LBrack)),
                just(Token::RBrack),
            )
            .map_with(|es, e| Located::new(Expr::Array(es), e.span()))
            .boxed();

        // `[a; b; c]` — a `List` literal. One `;` anywhere makes it a list, so a
        // single-element list is `[a;]`; without a `;` the brackets are a
        // `Vector`, which is why `[a]` cannot mean this.
        let list_expr = expr
            .clone()
            .then(
                just(Token::SemiColon)
                    .ignore_then(expr.clone().or_not())
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .map(|(head, tail)| {
                let mut es = vec![head];
                es.extend(tail.into_iter().flatten());
                es
            })
            // `[;]` — the empty list. `[]` is the empty `Vector`.
            .or(just(Token::SemiColon).map(|_| vec![]))
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|es, e| Located::new(Expr::List(es), e.span()))
            .boxed();

        // `[a, b, c]` — an RRB `Vector` literal (comma-separated). Desugars to
        // `vecFromArray #[a, b, c]` (`vecFromArray` comes from the prelude).
        let vec_expr = expr
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|es, e| {
                let span = e.span();
                let arr = Located::new(Expr::Array(es), span);
                let f = Located::new(
                    Expr::Var(Located::new(InternedString::from("vecFromArray"), span)),
                    span,
                );
                Located::new(Expr::App(f, vec![arr]), span)
            })
            .boxed();

        let bind = {
            // `let rec` is accepted but redundant: named bindings are already
            // self-recursive.
            let rec_prefix = just(Token::LowerIdent(InternedString::from("rec"))).or_not();

            let fun_bind = just(Token::Fun)
                .ignore_then(lower_ident())
                .then(param_pat().repeated().at_least(1).collect::<Vec<_>>())
                .then(result_ty())
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|(((name, args), ret), body)| bind_of(name, args, ret, body));

            // `[rec] name args = body`  /  `[rec] name = body`
            let name_bind = rec_prefix
                .ignore_then(lower_ident())
                .then(param_pat().repeated().collect::<Vec<_>>())
                .then(result_ty())
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|(((name, args), ret), body)| bind_of(name, args, ret, body));

            let pat_bind = pat()
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|(p, e)| Bind::Pat(p, e));

            choice((fun_bind, name_bind, pat_bind)).boxed()
        };

        let let_expr = just(Token::Let)
            .ignore_then(bind.clone().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::In))
            .then(expr.clone())
            .map(|(binds, body)| Expr::Let(binds, body))
            .map_with(|e, ex| Located::new(e, ex.span()))
            .boxed();

        let if_expr = just(Token::If)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Then))
            .then(expr.clone())
            .then_ignore(just(Token::Else))
            .then(expr.clone())
            .map(|((cond, then), else_)| Expr::If(cond, then, else_))
            .map_with(|e, ex| Located::new(e, ex.span()))
            .boxed();

        let lam_expr = just(Token::Backslash)
            .ignore_then(pat().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::RArrow))
            .then(expr.clone())
            .map(|(params, body)| Expr::Lam(params, body))
            .map_with(|e, ex| Located::new(e, ex.span()))
            .boxed();

        // `| p -> e`, or `| p if guard -> e`, taken only where the guard holds.
        let match_arm = just(Token::Bar)
            .ignore_then(pat())
            .then(just(Token::If).ignore_then(expr.clone()).or_not())
            .then_ignore(just(Token::RArrow))
            .then(expr.clone())
            .map(|((p, guard), body)| (p, guard, body));

        let match_expr = just(Token::Match)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::With))
            .then(match_arm.repeated().at_least(1).collect::<Vec<_>>())
            .map(|(scrutinee, branches)| Expr::Match(scrutinee, branches))
            .map_with(|e, ex| Located::new(e, ex.span()))
            .boxed();

        // `{ x = e, y = e | base }`; `{ x }` is shorthand for `{ x = x }`.
        let record_field = lower_ident()
            .then(just(Token::Eq).ignore_then(expr.clone()).or_not())
            .map(|(name, val)| {
                let val = val.unwrap_or_else(|| Located::new(Expr::Var(name.clone()), name.span));
                (name, val)
            });

        // `{ r | x = e, y = e }`: `r` with those fields replaced. Tried before the
        // `{ x = e | r }` extension, which it can only be mistaken for when its
        // fields have no `=`, and then it is not this.
        let update_field = lower_ident()
            .then_ignore(just(Token::Eq))
            .then(expr.clone());
        let update_expr = expr
            .clone()
            .then_ignore(just(Token::Bar))
            .then(
                update_field
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map_with(|(base, fields), e| Located::new(Expr::Update(base, fields), e.span()))
            .boxed();

        let record_expr = record_field
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .then(just(Token::Bar).ignore_then(expr.clone()).or_not())
            .delimited_by(just(Token::LBrace), just(Token::RBrace))
            .map_with(|(fields, base), e| Located::new(Expr::Record(fields, base), e.span()))
            .boxed();

        // `handle e with { op p k -> body, return x -> body }`  (arms comma-separated)
        let handle_expr = {
            let ret_arm = just(Token::LowerIdent(InternedString::from("return")))
                .ignore_then(pat())
                .then_ignore(just(Token::RArrow))
                .then(expr.clone())
                .map(Either::Right);
            let op_arm = lower_ident()
                .then(pat())
                .then(lower_ident())
                .then_ignore(just(Token::RArrow))
                .then(expr.clone())
                .map(|(((op, param), resume), body)| {
                    Either::Left(HandlerArm {
                        op,
                        param,
                        resume,
                        body,
                    })
                });
            just(Token::Handle)
                .ignore_then(expr.clone())
                .then_ignore(just(Token::With))
                .then(
                    choice((ret_arm, op_arm))
                        .separated_by(just(Token::Comma))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .delimited_by(just(Token::LBrace), just(Token::RBrace)),
                )
                .map_with(|(scrut, arms), e| {
                    let mut ops = Vec::new();
                    let mut ret = None;
                    for a in arms {
                        match a {
                            Either::Left(arm) => ops.push(arm),
                            Either::Right((x, body)) => ret = Some((x, body)),
                        }
                    }
                    Located::new(Expr::Handle(scrut, ops, ret), e.span())
                })
                .boxed()
        };

        // A bare constructor (`Nil`, `True`) is an atom so it can be a function or
        // constructor argument; `cons` below gathers its arguments when it has any.
        let ctor_atom =
            upper_ident().map_with(|n, e| Located::new(Expr::Cons(n, vec![]), e.span()));

        // `Mod.name` / `Mod.Ctor` as a bare atom (0-ary); `qual` below gathers args.
        let qual_atom = upper_ident()
            .then_ignore(just(Token::Period))
            .then(choice((lower_ident(), upper_ident())))
            .map_with(|(q, n), e| Located::new(Expr::Qual(q, n), e.span()));

        // `"a ${e} b"` -- the lexer has already split the literal into its text
        // and the tokens of each hole.
        let interp_expr = select! { Token::InterpStart(s) => s }
            .then(expr.clone())
            .then(
                select! { Token::InterpMid(s) => s }
                    .then(expr.clone())
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .then(select! { Token::InterpEnd(s) => s })
            .map_with(|(((first, hole), rest), last), e| {
                let mut texts = vec![first];
                let mut holes = vec![hole];
                for (text, hole) in rest {
                    texts.push(text);
                    holes.push(hole);
                }
                texts.push(last);
                Located::new(Expr::Interp(texts, holes), e.span())
            })
            .boxed();

        // `_` — an operator-section hole (see `desugar_section`).
        let hole_expr = just(Token::Wildcard).map_with(|_, e| Located::new(Expr::Hole, e.span()));

        // `( e )` — grouping, but if `e` contains `_` holes it's an operator
        // section and desugars to a lambda: `(_ + 1)` is `\h -> h + 1`.
        let section = expr
            .clone()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map(desugar_section)
            .boxed();

        let atom = choice((
            // Before every name: a call is a name, so the alternatives that
            // read one would take the name and leave the `!`. The probe costs
            // a token or two and never descends into a nested expression.
            located(mac_call().map(Expr::MacCall)),
            qual_atom,
            unit_expr,
            lit_expr,
            interp_expr,
            var_expr,
            hole_expr,
            ctor_atom,
            update_expr,
            record_expr,
            handle_expr,
            let_expr,
            if_expr,
            lam_expr,
            match_expr,
            section,
            tuple,
            range_list,
            array_expr,
            list_expr,
            vec_expr,
        ));

        // postfix `.field` selection
        let atom = atom
            .then(
                just(Token::Period)
                    .ignore_then(lower_ident())
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map_with(|(obj, fields), e| {
                fields.into_iter().fold(obj, |o, field| {
                    Located::new(Expr::Field(o, field), e.span())
                })
            })
            .boxed();

        // Zero-or-more arguments, and a bare atom when there are none — *not*
        // `at_least(1)` with `atom` as a later alternative in the `choice` below.
        //
        // That arrangement parses the head atom, discovers there is no argument,
        // throws the whole parse away and lets `atom` redo it. For a nested
        // expression that doubles the work at every level, so a `let` inside an
        // `if` inside a `let` costs 2^depth: `Std.Collections.Vector` took 3.6
        // seconds to parse, and a ten-deep nest took eighty.
        let app = atom
            .clone()
            .then(atom.clone().repeated().collect::<Vec<_>>())
            .map_with(|(f, args), e| {
                if args.is_empty() {
                    f
                } else {
                    Located::new(Expr::App(f, args), e.span())
                }
            })
            .boxed();

        // Constructor arguments are atoms, exactly like function-application
        // arguments — `Cons (f x) (map f xs)` is `Cons` applied to two args, not
        // `Cons ((f x) (map f xs))`.
        let cons = upper_ident()
            .then(
                atom.clone()
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<_>>()
                    .or_not(),
            )
            .map(|(name, args)| Expr::Cons(name, args.unwrap_or_default()))
            .map_with(|e, ex| Located::new(e, ex.span()))
            .boxed();

        // `Mod.name a b` / `Mod.Ctor a b` — a qualified name applied to arguments.
        let qual = upper_ident()
            .then_ignore(just(Token::Period))
            .then(choice((lower_ident(), upper_ident())))
            .then(atom.clone().repeated().collect::<Vec<_>>())
            .map_with(|((q, n), args), e| {
                let base = Located::new(Expr::Qual(q, n), e.span());
                if args.is_empty() {
                    base
                } else {
                    Located::new(Expr::App(base, args), e.span())
                }
            })
            .boxed();

        let ops = choice((qual, cons, app)).clone().pratt((
            prefix(6, just(Token::Minus), |_op: Token, exp: Located<Expr>, e| {
                let span = e.span();
                let inner_span = exp.span;
                // Fold `-<literal>` into a signed literal so `-1.5` works without
                // a `Float -> Float` `neg`; anything else stays `neg <expr>`.
                match *exp.value {
                    Expr::Lit(Lit::Float(bits)) => Located::new(
                        Expr::Lit(Lit::Float((-f64::from_bits(bits)).to_bits())),
                        span,
                    ),
                    Expr::Lit(Lit::Int(n)) => {
                        Located::new(Expr::Lit(Lit::Int(-n)), span)
                    }
                    other => Located::new(
                        Expr::UnOp(
                            Located::new(UnOp::Neg, span),
                            Located::new(other, inner_span),
                        ),
                        span,
                    ),
                }
            }),
            // `head :: tail` — sugar for `Cons head tail` (right-associative).
            infix(
                right(2),
                just(Token::ColonColon),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::Cons(
                            Located::new(InternedString::from("Cons"), e.span()),
                            vec![left, right],
                        ),
                        e.span(),
                    )
                },
            ),
            // `a ++ b` -- a user operator, so an application of whatever `++`
            // names in scope. Right-associative, beside `::`.
            infix(
                right(2),
                user_op(),
                |l: Located<Expr>, op: Ident, r: Located<Expr>, e| {
                    let f = Located::new(Expr::Var(op.clone()), op.span);
                    Located::new(Expr::App(f, vec![l, r]), e.span())
                },
            ),
            // Float operators: `*.` `/.` (tight), `+.` `-.` (loose), `<. >. <=. >=.`.
            infix(
                left(4),
                select! { Token::OpIdent(s) if &*s == "*." || &*s == "/." => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| float_binop(&s, l, r, e.span()),
            ),
            infix(
                left(3),
                select! { Token::OpIdent(s) if &*s == "+." || &*s == "-." => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| float_binop(&s, l, r, e.span()),
            ),
            infix(
                none(2),
                select! { Token::OpIdent(s) if matches!(&*s, "<." | ">." | "<=." | ">=.") => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| float_binop(&s, l, r, e.span()),
            ),
            // Bit shifts, on any integer type — same precedence as `+`/`-`, left-associative.
            infix(
                left(3),
                select! { Token::OpIdent(s) if matches!(&*s, "<<" | ">>" | ">>>") => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| bit_binop(&s, l, r, e.span()),
            ),
            // infix ops
            infix(
                left(3),
                just(Token::Plus),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Add, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                left(3),
                just(Token::Minus),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Sub, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                left(4),
                just(Token::Star),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Mul, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                left(4),
                just(Token::Slash),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Div, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                left(4),
                just(Token::Percent),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Mod, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                right(5),
                just(Token::Caret),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Pow, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                left(1),
                just(Token::EqEq),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Eq, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                none(1),
                just(Token::Neq),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Neq, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                none(2),
                just(Token::Lt),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Lt, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                none(2),
                just(Token::Gt),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Gt, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                none(2),
                just(Token::Leq),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Leq, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
            infix(
                none(2),
                just(Token::Geq),
                |left: Located<Expr>, _, right: Located<Expr>, e| {
                    Located::new(
                        Expr::BinOp(Located::new(BinOp::Geq, e.span()), left, right),
                        e.span(),
                    )
                },
            ),
        )).boxed();

        // `and` / `or` sit below every pratt operator and short-circuit (the
        // resolver turns them into `if`). `and` binds tighter than `or`.
        let bin = |op: BinOp| {
            move |l: Located<Expr>, r: Located<Expr>| {
                let span = l.span.extend(r.span);
                Located::new(Expr::BinOp(Located::new(op.clone(), span), l, r), span)
            }
        };
        let and_expr = ops
            .clone()
            .foldl(
                just(Token::And).ignore_then(ops.clone()).repeated(),
                bin(BinOp::And),
            )
            .boxed();
        let or_expr = and_expr
            .clone()
            .foldl(
                just(Token::Or).ignore_then(and_expr.clone()).repeated(),
                bin(BinOp::Or),
            )
            .boxed();

        // `x |> f` == `f x` (left-assoc); `f <| x` == `f x` (right-assoc, loosest).
        let app1 = |func: Located<Expr>, arg: Located<Expr>| {
            let span = func.span.extend(arg.span);
            Located::new(Expr::App(func, vec![arg]), span)
        };
        let pipe_l = or_expr
            .clone()
            .foldl(
                just(Token::RPipe).ignore_then(or_expr.clone()).repeated(),
                move |lhs, rhs| app1(rhs, lhs),
            )
            .boxed();
        pipe_l
            .clone()
            .then(
                just(Token::LPipe)
                    .ignore_then(pipe_l)
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map(move |(head, rest)| {
                if rest.is_empty() {
                    return head;
                }
                let mut all = vec![head];
                all.extend(rest);
                let last = all.pop().unwrap();
                all.into_iter().rev().fold(last, |acc, f| app1(f, acc))
            })
            .boxed()
    })
}

/// Build a float-operator `BinOp` node from its symbol (`"+."`, `"<=."`, …).
fn float_binop(sym: &str, l: LExpr, r: LExpr, span: Span) -> LExpr {
    let op = match sym {
        "+." => BinOp::AddF,
        "-." => BinOp::SubF,
        "*." => BinOp::MulF,
        "/." => BinOp::DivF,
        "<." => BinOp::LtF,
        ">." => BinOp::GtF,
        "<=." => BinOp::LeqF,
        ">=." => BinOp::GeqF,
        _ => unreachable!("float_binop: {sym}"),
    };
    Located::new(Expr::BinOp(Located::new(op, span), l, r), span)
}

/// Desugar a bit-shift operator (`<<` / `>>` / `>>>`) to a call of the
/// corresponding `Int` primitive (`shl` / `shr` / `ushr`).
fn bit_binop(sym: &str, l: LExpr, r: LExpr, span: Span) -> LExpr {
    let name = match sym {
        "<<" => "shl",
        ">>" => "shr",
        ">>>" => "ushr",
        _ => unreachable!("bit_binop: {sym}"),
    };
    let f = Located::new(
        Expr::Var(Located::new(InternedString::from(name), span)),
        span,
    );
    Located::new(Expr::App(f, vec![l, r]), span)
}

/// Turn a parenthesised expression into a lambda if it contains `_` holes:
/// `(_ + _)` becomes `\_hole0 _hole1 -> _hole0 + _hole1`, `(f _ 3)` becomes
/// `\_hole0 -> f _hole0 3`. Holes are numbered left-to-right in source order.
/// Holes inside a nested `\ …` belong to that lambda and are left alone. With no
/// holes the expression is returned unchanged (plain grouping).
fn desugar_section(inner: LExpr) -> LExpr {
    let span = inner.span;
    let mut n = 0usize;
    let body = fill_holes(inner, &mut n);
    if n == 0 {
        return body;
    }
    let params = (0..n)
        .map(|i| {
            Located::new(
                Pat::Var(Located::new(
                    InternedString::from(format!("_hole{i}")),
                    span,
                )),
                span,
            )
        })
        .collect();
    Located::new(Expr::Lam(params, body), span)
}

/// Replace each `Expr::Hole` with `Expr::Var(_hole{k})`, bumping `n`. Recurses
/// through every sub-expression except the body of a nested lambda / `fun` bind.
fn fill_holes(e: LExpr, n: &mut usize) -> LExpr {
    let span = e.span;
    let go = |x, n: &mut usize| fill_holes(x, n);
    let kind = match *e.value {
        Expr::Hole => {
            let name = InternedString::from(format!("_hole{n}"));
            *n += 1;
            Expr::Var(Located::new(name, span))
        }
        Expr::Var(v) => Expr::Var(v),
        Expr::Lit(l) => Expr::Lit(l),
        // The argument is tokens, not expressions: a `_` in there is the
        // macro's to make sense of once it has expanded.
        Expr::MacCall(m) => Expr::MacCall(m),
        Expr::Interp(texts, holes) => {
            Expr::Interp(texts, holes.into_iter().map(|x| go(x, n)).collect())
        }
        Expr::Unit => Expr::Unit,
        // A nested lambda owns any holes in its body.
        Expr::Lam(ps, b) => Expr::Lam(ps, b),
        Expr::App(f, args) => Expr::App(go(f, n), args.into_iter().map(|a| go(a, n)).collect()),
        Expr::UnOp(op, x) => Expr::UnOp(op, go(x, n)),
        Expr::BinOp(op, l, r) => Expr::BinOp(op, go(l, n), go(r, n)),
        Expr::Tuple(xs) => Expr::Tuple(xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::Array(xs) => Expr::Array(xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::List(xs) => Expr::List(xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::Cons(name, xs) => Expr::Cons(name, xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::Qual(q, name) => Expr::Qual(q, name),
        Expr::Field(o, l) => Expr::Field(go(o, n), l),
        Expr::Update(base, fields) => Expr::Update(
            go(base, n),
            fields.into_iter().map(|(l, v)| (l, go(v, n))).collect(),
        ),
        Expr::Record(fields, base) => Expr::Record(
            fields.into_iter().map(|(l, v)| (l, go(v, n))).collect(),
            base.map(|b| go(b, n)),
        ),
        Expr::If(c, t, f) => Expr::If(go(c, n), go(t, n), go(f, n)),
        Expr::Match(s, arms) => Expr::Match(
            go(s, n),
            arms.into_iter()
                .map(|(p, g, b)| (p, g.map(|g| go(g, n)), go(b, n)))
                .collect(),
        ),
        Expr::Let(binds, body) => Expr::Let(
            binds
                .into_iter()
                .map(|b| match b {
                    Bind::Pat(p, x) => Bind::Pat(p, go(x, n)),
                    // a `fun` bind owns its body's holes
                    other => other,
                })
                .collect(),
            go(body, n),
        ),
        Expr::Handle(s, arms, ret) => Expr::Handle(
            go(s, n),
            arms.into_iter()
                .map(|mut a| {
                    a.body = go(a.body, n);
                    a
                })
                .collect(),
            ret.map(|(p, b)| (p, go(b, n))),
        ),
    };
    Located::new(kind, span)
}

fn pat<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LPat, extra::Err<Rich<'a, Token, Span>>> + Clone {
    recursive(|pat| {
        // `[a, b]` / `[a; b]` — a `List` pattern (either separator). There is no
        // A `;` anywhere makes a bracket pattern a `List`, exactly as it does for
        // the literal and the type — so `[;]` is `Nil` and `[x;]` is one element.
        // Without one the brackets are a `Vector`, of which only `[]` is
        // matchable; the resolver reports the rest.
        let list = pat
            .clone()
            .then(
                just(Token::SemiColon)
                    .ignore_then(pat.clone().or_not())
                    .repeated()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .map(|(head, tail)| {
                let mut ps = vec![head];
                ps.extend(tail.into_iter().flatten());
                Pat::List(ps)
            })
            .or(just(Token::SemiColon).map(|_| Pat::List(vec![])))
            .or(pat
                .clone()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect()
                .map(Pat::Vector))
            .delimited_by(just(Token::LBrack), just(Token::RBrack));

        // `#[p, ...]` — a builtin `Array` pattern (exact length).
        let array = just(Token::Hash)
            .ignore_then(just(Token::LBrack))
            .ignore_then(
                pat.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect(),
            )
            .then_ignore(just(Token::RBrack))
            .map(|patterns| Pat::Array(patterns));

        // `(p : T)` — an annotated pattern, which is how a parameter gets a
        // declared type. Tried before the grouping rule below, which would
        // otherwise consume the `(p` and then fail on the colon; chumsky
        // backtracks, so the ordering is all that is needed.
        //
        // The parentheses are not decoration. A pattern is a *parameter* in
        // `fun f p q = …`, so without them `fun f x : Int = …` would have no
        // way to say whether the annotation belongs to `x` or to `f`.
        let annotated = just(Token::LParen)
            .ignore_then(pat.clone())
            .then_ignore(just(Token::Colon))
            .then(ty())
            .then_ignore(just(Token::RParen))
            .map(|(p, t)| Pat::Ann(Box::new(p), t));

        // `(p)` is grouping, `(p, q)` a tuple — mirroring the type grammar.
        let tuple = just(Token::LParen)
            .ignore_then(
                pat.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just(Token::RParen))
            .map(|mut patterns| match patterns.len() {
                // `()` is the unit pattern, not a tuple of nothing — the same
                // reading `param_pat` gives it. They have to agree: a lambda's
                // parameter goes through this parser and a `fun`'s through
                // that one, and `\() -> e` would otherwise take an argument
                // that nothing could be passed to.
                0 => Pat::Unit,
                1 => *patterns.pop().unwrap().value,
                _ => Pat::Tuple(patterns),
            });

        let record_field = lower_ident()
            .then(just(Token::Eq).ignore_then(pat.clone()).or_not())
            .map(|(name, p)| {
                let p = p.unwrap_or_else(|| Located::new(Pat::Var(name.clone()), name.span));
                (name, p)
            });

        let record = just(Token::LBrace)
            .ignore_then(
                record_field
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect::<Vec<_>>(),
            )
            .then(just(Token::Bar).ignore_then(just(Token::Wildcard)).or_not())
            .then_ignore(just(Token::RBrace))
            .map(|(fields, open)| Pat::Record(fields, open.is_some()));

        // What can stand on its own, and so be a constructor's argument: a
        // constructor written there takes no arguments of its own, so
        // `Node Leaf x r` is `Node` applied to three patterns, as in Haskell.
        // A constructor that does take some is parenthesized, `Just (Cons x r)`.
        let argument = mac_call()
            .map(Pat::MacCall)
            .or(upper_ident()
                .then_ignore(just(Token::Period))
                .then(upper_ident())
                .map(|(q, name)| Pat::QualCons(q, name, Vec::new())))
            .or(upper_ident().map(|name| Pat::Cons(name, Vec::new())))
            .or(record)
            .or(value_ident().map(|ident| Pat::Var(ident)))
            .or(just(Token::Wildcard).map(|_| Pat::Wildcard))
            .or(lit().map(Pat::Lit))
            .or(array)
            .or(list)
            .or(annotated)
            .or(tuple)
            .or(unit().map(|_| Pat::Unit))
            .map_with(|kind, e| LPat::new(kind, e.span()))
            .boxed();

        // `Mod.Ctor p q` — a constructor pattern qualified by a `use`d module.
        let qual_cons = upper_ident()
            .then_ignore(just(Token::Period))
            .then(upper_ident())
            .then(argument.clone().repeated().at_least(1).collect::<Vec<_>>())
            .map(|((q, name), args)| Pat::QualCons(q, name, args));

        let cons = upper_ident()
            .then(argument.clone().repeated().at_least(1).collect::<Vec<_>>())
            .map(|(name, args)| Pat::Cons(name, args));

        let atom = qual_cons
            .or(cons)
            .map_with(|kind, e| LPat::new(kind, e.span()))
            .or(argument)
            .boxed();

        // `a :: b :: rest` — sugar for `Cons a (Cons b rest)`, grouping to the
        // right. Its parts are atoms rather than whole patterns, so that an
        // `as` after the tail names the whole chain rather than the tail.
        let consed = atom
            .clone()
            .separated_by(just(Token::ColonColon))
            .at_least(1)
            .collect::<Vec<_>>()
            .map(|mut parts| {
                let mut tail = parts.pop().expect("at least one");
                while let Some(head) = parts.pop() {
                    let span = Span::from(head.span.start as usize..tail.span.end as usize);
                    tail = LPat::new(
                        Pat::Cons(
                            Located::new(InternedString::from("Cons"), span),
                            vec![head, tail],
                        ),
                        span,
                    );
                }
                tail
            });

        // `p as x` binds loosest, as in OCaml: `x :: rest as whole` names the
        // whole list, and `(x as y) :: rest` a part of it.
        consed
            .then(
                just(Token::As)
                    .ignore_then(value_ident())
                    .repeated()
                    .collect::<Vec<_>>(),
            )
            .map_with(|(p, names), e| {
                names
                    .into_iter()
                    .fold(p, |p, name| LPat::new(Pat::As(name, p), e.span()))
            })
            .boxed()
    })
}

/// A **parameter** pattern: the unambiguous, atomic subset of [`pat`], so
/// `fun f (a, b) c = e` reads as two parameters rather than one constructor
/// application. Anything richer goes in parentheses — `fun f (Just x) = e`
/// parses fine and is then rejected by the irrefutability check, which is where
/// the useful error lives (see `meadow-exhaust`).
///
/// Atomic includes literals and constructors that take nothing, which is what a
/// function written as several equations matches on:
///
/// ```text
/// fun gcd a 0 = a
///   | gcd a b = gcd b (a % b)
/// ```
///
/// A single equation may use them too, and is then simply refutable, which the
/// exhaustiveness check reports.
fn param_pat<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LPat, extra::Err<Rich<'a, Token, Span>>> + Clone {
    let inner = pat();

    // `(p : T)` — a parameter with a declared type. This parser deliberately
    // does not reuse `pat`'s outermost rules (a bare `Just x` here would read as
    // two parameters), so the annotated form has to be repeated rather than
    // inherited. Before `paren`, which would otherwise take the `(p` and fail.
    let annotated = just(Token::LParen)
        .ignore_then(inner.clone())
        .then_ignore(just(Token::Colon))
        .then(ty())
        .then_ignore(just(Token::RParen))
        .map_with(|(p, t), e| LPat::new(Pat::Ann(Box::new(p), t), e.span()));

    // `()` unit, `(p)` grouping, `(p, q)` tuple.
    let paren = inner
        .clone()
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(just(Token::LParen), just(Token::RParen))
        .map_with(|mut ps, e| match ps.len() {
            0 => LPat::new(Pat::Unit, e.span()),
            1 => ps.pop().unwrap(),
            _ => LPat::new(Pat::Tuple(ps), e.span()),
        });

    let record_field = lower_ident()
        .then(just(Token::Eq).ignore_then(inner.clone()).or_not())
        .map(|(name, p)| {
            let p = p.unwrap_or_else(|| Located::new(Pat::Var(name.clone()), name.span));
            (name, p)
        });
    let record = record_field
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .collect::<Vec<_>>()
        .then(just(Token::Bar).ignore_then(just(Token::Wildcard)).or_not())
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map_with(|(fields, open), e| LPat::new(Pat::Record(fields, open.is_some()), e.span()));

    // `0`, `"x"`, `'c'`. A literal cannot be the head of an application, so it
    // is as atomic as a name is -- and it is what a function written as several
    // equations matches on.
    let literal = lit().map_with(|l, e| LPat::new(Pat::Lit(l), e.span()));

    // `Nothing`, `Maybe.Nothing` -- a constructor taking nothing. One that takes
    // something is written `(Just x)`, exactly as in Haskell, and for the same
    // reason: bare, it would read as two parameters.
    let nullary = upper_ident()
        .then_ignore(just(Token::Period))
        .then(upper_ident())
        .map_with(|(q, n), e| LPat::new(Pat::QualCons(q, n, Vec::new()), e.span()))
        .or(upper_ident().map_with(|n, e| LPat::new(Pat::Cons(n, Vec::new()), e.span())));

    // `[;]`, `[x; xs]`, `[]`, `#[a, b]`. Brackets say where these end, so the
    // whole pattern grammar can be used inside them without ambiguity.
    let bracketed = just(Token::LBrack)
        .or(just(Token::Hash))
        .rewind()
        .ignore_then(inner.clone());

    choice((
        annotated,
        paren,
        record,
        bracketed,
        nullary,
        literal,
        value_ident().map_with(|n, e| LPat::new(Pat::Var(n), e.span())),
        just(Token::Wildcard).map_with(|_, e| LPat::new(Pat::Wildcard, e.span())),
    ))
    .boxed()
}

fn unit<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, (), extra::Err<Rich<'a, Token, Span>>> + Clone {
    just(Token::LParen)
        .ignore_then(just(Token::RParen))
        .map_with(|_, _| ())
}

fn lower_ident<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> + Clone {
    select! {
        Token::LowerIdent(name) => name
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

/// The operators a program can bind a value to, with `fun (++) a b = ...` or
/// `def (++) = ...`, and then use infix. Every other operator is a primitive
/// with a fixed meaning; these are ordinary names that happen to be written
/// with symbols, so they are scoped, exported and imported like any other.
pub const USER_OPERATORS: &[&str] = &["++"];

/// A user-bindable operator token, as the name it binds.
fn user_op<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> + Clone {
    select! {
        Token::OpIdent(name) if USER_OPERATORS.contains(&&*name) => name
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

/// A name a value is bound to: `x`, or an operator in parentheses, `(++)`.
fn value_ident<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> + Clone {
    lower_ident().or(user_op().delimited_by(just(Token::LParen), just(Token::RParen)))
}

fn upper_ident<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> + Clone {
    select! {
        Token::UpperIdent(name) => name
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

fn lit<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Lit, extra::Err<Rich<'a, Token, Span>>> + Clone {
    select! {
        Token::Int(i) => Lit::Int(i),
        Token::Real(bits) => Lit::Float(bits),
        Token::String(s) => Lit::String(s),
        Token::Char(c) => Lit::Char(c),
    }
}

fn located<'tokens, I, P>(
    p: P,
) -> impl Parser<'tokens, I, LExpr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
    P: Parser<'tokens, I, Expr, extra::Err<Rich<'tokens, Token, Span>>> + Clone,
{
    p.map_with(|v, e| Located::new(v, e.span()))
}

/// `: T` before the `=` of a binding — the declared result type.
fn result_ty<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Option<LType>, extra::Err<Rich<'a, Token, Span>>> + Clone {
    just(Token::Colon).ignore_then(ty()).or_not()
}

/// One equation of a function written in several: `| gcd a b = …`.
struct Clause {
    name: Ident,
    args: Vec<LPat>,
    body: LExpr,
}

/// Assemble a function written as several equations.
///
/// ```text
/// fun gcd a 0 = a
///   | gcd a b = gcd b (a % b)
/// ```
///
/// becomes one function whose body matches on its arguments:
///
/// ```text
/// fun gcd _arg0 _arg1 =
///   match (_arg0, _arg1) with
///   | (a, 0) -> a
///   | (a, b) -> gcd b (a % b)
/// ```
///
/// which is sugar and nothing more -- so the equations are checked for
/// exhaustiveness, and overlap, exactly as the `match` a person would have
/// written by hand. One equation is left alone, and compiles to what it always
/// did.
fn equations<'a>(
    name: Ident,
    args: Vec<LPat>,
    ret: Option<LType>,
    body: LExpr,
    rest: Vec<Clause>,
    span: Span,
    emitter: &mut chumsky::input::Emitter<Rich<'a, Token, Span>>,
) -> Bind {
    if rest.is_empty() {
        return bind_of(name, args, ret, body);
    }
    // Nothing to match on, so the equations could not be told apart.
    if args.is_empty() {
        emitter.emit(Rich::custom(
            span,
            format!(
                "`{}` is written as several equations but takes no arguments, so there is nothing to tell them apart by",
                name.value()
            ),
        ));
        return bind_of(name, args, ret, body);
    }
    for c in &rest {
        if c.name.value() != name.value() {
            emitter.emit(Rich::custom(
                c.name.span,
                format!(
                    "this equation defines `{}`, but the ones above it define `{}`",
                    c.name.value(),
                    name.value()
                ),
            ));
        }
        if c.args.len() != args.len() {
            emitter.emit(Rich::custom(
                c.name.span,
                format!(
                    "this equation of `{}` takes {} argument{}, but the first takes {}",
                    name.value(),
                    c.args.len(),
                    if c.args.len() == 1 { "" } else { "s" },
                    args.len()
                ),
            ));
        }
    }

    // One fresh name per argument, to match on. A name a person could write
    // would be shadowed by whatever each equation binds, so these need not be
    // unforgeable -- only unlikely, and `_` keeps them out of the way.
    let params: Vec<LPat> = (0..args.len())
        .map(|i| {
            Located::new(
                Pat::Var(Ident::new(
                    InternedString::from(format!("_arg{i}")),
                    name.span,
                )),
                name.span,
            )
        })
        .collect();
    let scrutinee = tuple_of(
        params
            .iter()
            .map(|p| match p.value() {
                Pat::Var(n) => Located::new(Expr::Var(n.clone()), n.span),
                _ => unreachable!("the parameters just made are variables"),
            })
            .collect(),
        name.span,
    );

    let arm = |pats: Vec<LPat>, body: LExpr| {
        let span = body.span;
        (pat_tuple_of(pats, span), None, body)
    };
    let mut arms = vec![arm(args, body)];
    arms.extend(rest.into_iter().map(|c| arm(c.args, c.body)));

    let matched = Located::new(Expr::Match(scrutinee, arms), span);
    Bind::Fun(name, params, ret, matched)
}

/// One expression, or a tuple of several.
fn tuple_of(mut xs: Vec<LExpr>, span: Span) -> LExpr {
    match xs.len() {
        1 => xs.pop().expect("one"),
        _ => Located::new(Expr::Tuple(xs), span),
    }
}

/// The same, for patterns.
fn pat_tuple_of(mut ps: Vec<LPat>, span: Span) -> LPat {
    match ps.len() {
        1 => ps.pop().expect("one"),
        _ => Located::new(Pat::Tuple(ps), span),
    }
}

/// Assemble a binding, which is a function only when it takes arguments.
///
/// `def x : Int = 5` has no parameters, so there is no result to declare — the
/// annotation is describing `x` itself, and the binding it belongs on is the
/// pattern. Routing it there rather than rejecting it means one syntax reads
/// the way it looks in both places.
fn bind_of(name: Ident, args: Vec<LPat>, ret: Option<LType>, body: LExpr) -> Bind {
    if !args.is_empty() {
        return Bind::Fun(name, args, ret, body);
    }
    let span = name.span;
    let var = Located::new(Pat::Var(name), span);
    match ret {
        Some(t) => Bind::Pat(Located::new(Pat::Ann(Box::new(var), t), span), body),
        None => Bind::Pat(var, body),
    }
}

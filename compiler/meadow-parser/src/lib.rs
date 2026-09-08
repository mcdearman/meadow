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

use meadow_ast::*;
use meadow_intern::InternedString;
use meadow_lexer::{LToken, Token};
use meadow_source::Source;
use meadow_span::{Located, Span};
use itertools::Either;
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
    let p = choice((
        decl().map(Either::Left),
        expr().map(Either::Right),
    ));
    p.parse(stream).into_output_errors()
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

/// `@pub`, `@attr(A, B, C)` — a `@` then a name then an optional parenthesised
/// list of argument names.
fn attr<'tokens, I>()
-> impl Parser<'tokens, I, Attr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    just(Token::At)
        .ignore_then(path_seg())
        .then(
            path_seg()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LParen), just(Token::RParen))
                .or_not(),
        )
        .map(|(name, args)| Attr {
            name,
            args: args.unwrap_or_default(),
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
        let pat_bind = just(Token::Def)
            .ignore_then(pat())
            .then_ignore(just(Token::Eq))
            .then(expr())
            .map(|(p, e)| Bind::Pat(p, e));

        // `fun f a (x, y) = e`, or point-free `fun f = e` (no parameters) — the
        // latter is just a value binding, so it takes the `Bind::Pat` path (and
        // its right-hand side is subject to the value restriction, like `def`).
        let fun_bind = just(Token::Fun)
            .ignore_then(lower_ident())
            .then(param_pat().repeated().collect::<Vec<_>>())
            .then_ignore(just(Token::Eq))
            .then(expr())
            .map(|((name, args), body)| {
                if args.is_empty() {
                    Bind::Pat(Located::new(Pat::Var(name.clone()), name.span), body)
                } else {
                    Bind::Fun(name, args, body)
                }
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
        // `as C` — rename the qualifier. Upper-case, because that is what a
        // qualified reference (`C.map`) can name.
        .then(just(Token::As).ignore_then(upper_ident()).or_not())
        .then(
            path_seg()
                .separated_by(just(Token::Comma))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(just(Token::LParen), just(Token::RParen))
                .or_not(),
        )
        .map_with(|((path, alias), names), e| {
            LDecl::new(
                Decl::Use(UseDecl {
                    path,
                    names: names.unwrap_or_default(),
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
            LDecl::new(
                Decl::Effect(EffectDecl { name, params, ops }),
                e.span(),
            )
        });

    choice((
        mod_decl,
        use_decl,
        data_decl,
        record_decl,
        effect_decl,
        bind_decl.map_with(|bind, e| LDecl::new(Decl::Bind(bind), e.span())),
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

        let tvar = lower_ident().map_with(|n, e| Located::new(TypeExpr::Var(n), e.span()));
        let tcon0 = upper_ident().map_with(|n, e| Located::new(TypeExpr::Con(n, vec![]), e.span()));

        let atom = choice((unit, seq, paren_or_tuple, tvar, tcon0));

        let app = upper_ident()
            .then(atom.clone().repeated().at_least(1).collect::<Vec<_>>())
            .map_with(|(n, args), e| Located::new(TypeExpr::Con(n, args), e.span()));

        let head = choice((app, atom.clone()));

        // effect annotation after `!` — reuses the local `atom` for label args,
        // so it must live inside this `recursive` closure (no separate fn).
        let eff_label = upper_ident()
            .then(atom.clone().repeated().collect::<Vec<_>>());
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
    let tvar = lower_ident().map_with(|n, e| Located::new(TypeExpr::Var(n), e.span()));
    let tcon0 = upper_ident().map_with(|n, e| Located::new(TypeExpr::Con(n, vec![]), e.span()));
    choice((unit, seq, paren_or_tuple, tvar, tcon0))
}

fn path_seg<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> + Clone {
    select! {
        Token::LowerIdent(name) => name,
        Token::UpperIdent(name) => name,
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

fn expr<'tokens, I>()
-> impl Parser<'tokens, I, LExpr, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    recursive(|expr| {
        let lit_expr = located(lit().map(Expr::Lit));
        let var_expr = located(lower_ident().map(Expr::Var));
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
            .then(choice((just(Token::DoublePeriod), just(Token::DoublePeriodEq))))
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
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|((name, args), body)| Bind::Fun(name, args, body));

            // `[rec] name args = body`  /  `[rec] name = body`
            let name_bind = rec_prefix
                .ignore_then(lower_ident())
                .then(param_pat().repeated().collect::<Vec<_>>())
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|((name, args), body)| {
                    if args.is_empty() {
                        Bind::Pat(Located::new(Pat::Var(name.clone()), name.span), body)
                    } else {
                        Bind::Fun(name, args, body)
                    }
                });

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

        let match_arm = just(Token::Bar)
            .ignore_then(pat())
            .then_ignore(just(Token::RArrow))
            .then(expr.clone());

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
                let val = val.unwrap_or_else(|| {
                    Located::new(Expr::Var(name.clone()), name.span)
                });
                (name, val)
            });

        let record_expr = record_field
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .collect::<Vec<_>>()
            .then(
                just(Token::Bar)
                    .ignore_then(expr.clone())
                    .or_not(),
            )
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

        // `_` — an operator-section hole (see `desugar_section`).
        let hole_expr = just(Token::Wildcard)
            .map_with(|_, e| Located::new(Expr::Hole, e.span()));

        // `( e )` — grouping, but if `e` contains `_` holes it's an operator
        // section and desugars to a lambda: `(_ + 1)` is `\h -> h + 1`.
        let section = expr
            .clone()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map(desugar_section)
            .boxed();

        let atom = choice((
            qual_atom,
            unit_expr,
            lit_expr,
            var_expr,
            hole_expr,
            ctor_atom,
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
                fields
                    .into_iter()
                    .fold(obj, |o, field| Located::new(Expr::Field(o, field), e.span()))
            })
            .boxed();

        let app = atom
            .clone()
            .then(atom.clone().repeated().at_least(1).collect::<Vec<_>>())
            .map_with(|(f, args), e| Located::new(Expr::App(f, args), e.span()))
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

        let ops = choice((qual, cons, app, atom)).clone().pratt((
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
            // BigInt operators: `*~` `/~` `%~` (tight), `+~` `-~` (loose), `^~`
            // (right-assoc, tightest), `<~ >~ <=~ >=~` (non-assoc comparisons).
            infix(
                left(4),
                select! { Token::OpIdent(s) if matches!(&*s, "*~" | "/~" | "%~") => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| big_binop(&s, l, r, e.span()),
            ),
            infix(
                left(3),
                select! { Token::OpIdent(s) if matches!(&*s, "+~" | "-~") => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| big_binop(&s, l, r, e.span()),
            ),
            infix(
                right(5),
                select! { Token::OpIdent(s) if &*s == "^~" => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| big_binop(&s, l, r, e.span()),
            ),
            infix(
                none(2),
                select! { Token::OpIdent(s) if matches!(&*s, "<~" | ">~" | "<=~" | ">=~") => s },
                |l: Located<Expr>, s: InternedString, r: Located<Expr>, e| big_binop(&s, l, r, e.span()),
            ),
            // Bit shifts on `Int` — same precedence as `+`/`-`, left-associative.
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
                Located::new(
                    Expr::BinOp(Located::new(op.clone(), span), l, r),
                    span,
                )
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

/// Build a BigInt-operator `BinOp` node from its symbol (`"+~"`, `"<=~"`, …).
fn big_binop(sym: &str, l: LExpr, r: LExpr, span: Span) -> LExpr {
    let op = match sym {
        "+~" => BinOp::AddB,
        "-~" => BinOp::SubB,
        "*~" => BinOp::MulB,
        "/~" => BinOp::DivB,
        "%~" => BinOp::ModB,
        "^~" => BinOp::PowB,
        "<~" => BinOp::LtB,
        ">~" => BinOp::GtB,
        "<=~" => BinOp::LeqB,
        ">=~" => BinOp::GeqB,
        _ => unreachable!("big_binop: {sym}"),
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
                Pat::Var(Located::new(InternedString::from(format!("_hole{i}")), span)),
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
        Expr::Unit => Expr::Unit,
        // A nested lambda owns any holes in its body.
        Expr::Lam(ps, b) => Expr::Lam(ps, b),
        Expr::App(f, args) => Expr::App(
            go(f, n),
            args.into_iter().map(|a| go(a, n)).collect(),
        ),
        Expr::UnOp(op, x) => Expr::UnOp(op, go(x, n)),
        Expr::BinOp(op, l, r) => Expr::BinOp(op, go(l, n), go(r, n)),
        Expr::Tuple(xs) => Expr::Tuple(xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::Array(xs) => Expr::Array(xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::List(xs) => Expr::List(xs.into_iter().map(|x| go(x, n)).collect()),
        Expr::Cons(name, xs) => {
            Expr::Cons(name, xs.into_iter().map(|x| go(x, n)).collect())
        }
        Expr::Qual(q, name) => Expr::Qual(q, name),
        Expr::Field(o, l) => Expr::Field(go(o, n), l),
        Expr::Record(fields, base) => Expr::Record(
            fields.into_iter().map(|(l, v)| (l, go(v, n))).collect(),
            base.map(|b| go(b, n)),
        ),
        Expr::If(c, t, f) => Expr::If(go(c, n), go(t, n), go(f, n)),
        Expr::Match(s, arms) => Expr::Match(
            go(s, n),
            arms.into_iter().map(|(p, b)| (p, go(b, n))).collect(),
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
                1 => *patterns.pop().unwrap().value,
                _ => Pat::Tuple(patterns),
            });

        // `Mod.Ctor p q` — a constructor pattern qualified by a `use`d module.
        let qual_cons = upper_ident()
            .then_ignore(just(Token::Period))
            .then(upper_ident())
            .then(pat.clone().repeated().collect::<Vec<_>>())
            .map(|((q, name), args)| Pat::QualCons(q, name, args));

        let cons = upper_ident()
            .then(pat.clone().repeated().collect::<Vec<_>>())
            .map(|(name, args)| Pat::Cons(name, args));

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
            .then(
                just(Token::Bar)
                    .ignore_then(just(Token::Wildcard))
                    .or_not(),
            )
            .then_ignore(just(Token::RBrace))
            .map(|(fields, open)| Pat::Record(fields, open.is_some()));

        let atom = qual_cons
            .or(cons)
            .or(record)
            .or(lower_ident().map(|ident| Pat::Var(ident)))
            .or(just(Token::Wildcard).map(|_| Pat::Wildcard))
            .or(lit().map(Pat::Lit))
            .or(array)
            .or(list)
            .or(tuple)
            .or(unit().map(|_| Pat::Unit))
            .map_with(|kind, e| LPat::new(kind, e.span()))
            .boxed();

        // `head :: tail` — sugar for the `Cons head tail` pattern (right-assoc).
        atom.clone()
            .then(
                just(Token::ColonColon)
                    .ignore_then(pat.clone())
                    .or_not(),
            )
            .map_with(|(head, tail), e| match tail {
                Some(tail) => LPat::new(
                    Pat::Cons(
                        Located::new(InternedString::from("Cons"), e.span()),
                        vec![head, tail],
                    ),
                    e.span(),
                ),
                None => head,
            })
            .boxed()
    })
}

/// A **parameter** pattern: the unambiguous, atomic subset of [`pat`], so
/// `fun f (a, b) c = e` reads as two parameters rather than one constructor
/// application. Anything richer goes in parentheses — `fun f (Just x) = e`
/// parses fine and is then rejected by the irrefutability check, which is where
/// the useful error lives (see `meadow-exhaust`).
fn param_pat<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LPat, extra::Err<Rich<'a, Token, Span>>> + Clone {
    let inner = pat();

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
        .then(
            just(Token::Bar)
                .ignore_then(just(Token::Wildcard))
                .or_not(),
        )
        .delimited_by(just(Token::LBrace), just(Token::RBrace))
        .map_with(|(fields, open), e| LPat::new(Pat::Record(fields, open.is_some()), e.span()));

    choice((
        paren,
        record,
        lower_ident().map_with(|n, e| LPat::new(Pat::Var(n), e.span())),
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
        Token::Real(x) => Lit::Float(x.to_bits()),
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

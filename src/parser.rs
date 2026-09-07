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
//! [`Diagnostic`]: crate::diagnostics::Diagnostic

use crate::{
    ast::*,
    intern::InternedString,
    lexer::{LToken, Token},
    source::Source,
    span::{Located, Span},
};
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

fn decl<'tokens, I>()
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

        let fun_bind = just(Token::Fun)
            .ignore_then(lower_ident())
            .then(lower_ident().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::Eq))
            .then(expr())
            .map(|((name, args), body)| Bind::Fun(name, args, body));

        fun_bind.or(pat_bind)
    };

    let use_decl = just(Token::Use)
        .ignore_then(
            path_seg()
                .separated_by(just(Token::Period))
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .map_with(|segs, e| LDecl::new(Decl::Use(segs), e.span()));

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
        use_decl,
        data_decl,
        record_decl,
        effect_decl,
        bind_decl.map_with(|bind, e| LDecl::new(Decl::Bind(bind), e.span())),
    ))
}

/// `{ name : Type, age : Type, }` — shared by `record` decls and named variant fields.
fn field_list<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Vec<(Ident, LType)>, extra::Err<Rich<'a, Token, Span>>> + Clone {
    lower_ident()
        .then_ignore(just(Token::Colon))
        .then(ty())
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

        let list = ty
            .clone()
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|t, e| Located::new(TypeExpr::List(t), e.span()));

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

        let atom = choice((unit, list, paren_or_tuple, tvar, tcon0));

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
    let list = inner
        .clone()
        .delimited_by(just(Token::LBrack), just(Token::RBrack))
        .map_with(|t, e| Located::new(TypeExpr::List(t), e.span()));
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
    choice((unit, list, paren_or_tuple, tvar, tcon0))
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
            .map_with(|es: Vec<LExpr>, e| Located::new(Expr::Tuple(es), e.span()));

        let list_expr = expr
            .clone()
            .separated_by(just(Token::Comma))
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|es, e| Located::new(Expr::List(es), e.span()));

        let bind = {
            // `let rec` is accepted but redundant: named bindings are already
            // self-recursive.
            let rec_prefix = just(Token::LowerIdent(InternedString::from("rec"))).or_not();

            let fun_bind = just(Token::Fun)
                .ignore_then(lower_ident())
                .then(lower_ident().repeated().at_least(1).collect::<Vec<_>>())
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|((name, args), body)| Bind::Fun(name, args, body));

            // `[rec] name args = body`  /  `[rec] name = body`
            let name_bind = rec_prefix
                .ignore_then(lower_ident())
                .then(lower_ident().repeated().collect::<Vec<_>>())
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

            choice((fun_bind, name_bind, pat_bind))
        };

        let let_expr = just(Token::Let)
            .ignore_then(bind.clone().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::In))
            .then(expr.clone())
            .map(|(binds, body)| Expr::Let(binds, body))
            .map_with(|e, ex| Located::new(e, ex.span()));

        let if_expr = just(Token::If)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Then))
            .then(expr.clone())
            .then_ignore(just(Token::Else))
            .then(expr.clone())
            .map(|((cond, then), else_)| Expr::If(cond, then, else_))
            .map_with(|e, ex| Located::new(e, ex.span()));

        let lam_expr = just(Token::Backslash)
            .ignore_then(pat().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::RArrow))
            .then(expr.clone())
            .map(|(params, body)| Expr::Lam(params, body))
            .map_with(|e, ex| Located::new(e, ex.span()));

        let match_arm = just(Token::Bar)
            .ignore_then(pat())
            .then_ignore(just(Token::RArrow))
            .then(expr.clone());

        let match_expr = just(Token::Match)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::With))
            .then(match_arm.repeated().at_least(1).collect::<Vec<_>>())
            .map(|(scrutinee, branches)| Expr::Match(scrutinee, branches))
            .map_with(|e, ex| Located::new(e, ex.span()));

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
            .map_with(|(fields, base), e| Located::new(Expr::Record(fields, base), e.span()));

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
        };

        // A bare constructor (`Nil`, `True`) is an atom so it can be a function or
        // constructor argument; `cons` below gathers its arguments when it has any.
        let ctor_atom =
            upper_ident().map_with(|n, e| Located::new(Expr::Cons(n, vec![]), e.span()));

        let atom = choice((
            unit_expr,
            lit_expr,
            var_expr,
            ctor_atom,
            record_expr,
            handle_expr,
            let_expr,
            if_expr,
            lam_expr,
            match_expr,
            expr.clone()
                .delimited_by(just(Token::LParen), just(Token::RParen)),
            tuple,
            list_expr,
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
            });

        let app = atom
            .clone()
            .then(atom.clone().repeated().at_least(1).collect::<Vec<_>>())
            .map_with(|(f, args), e| Located::new(Expr::App(f, args), e.span()));

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
            .map_with(|e, ex| Located::new(e, ex.span()));

        choice((cons, app, atom)).clone().pratt((
            prefix(1, just(Token::Minus), |op: Token, exp: Located<Expr>, e| {
                Located::new(
                    Expr::UnOp(Located::new(UnOp::from(op), e.span()), exp),
                    e.span(),
                )
            }),
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
        ))
    })
}

fn pat<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LPat, extra::Err<Rich<'a, Token, Span>>> + Clone {
    recursive(|pat| {
        let list = just(Token::LBrack)
            .ignore_then(
                pat.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect(),
            )
            .then_ignore(just(Token::RBrack))
            .map(|patterns| Pat::List(patterns));

        let tuple = just(Token::LParen)
            .ignore_then(
                pat.clone()
                    .separated_by(just(Token::Comma))
                    .allow_trailing()
                    .collect(),
            )
            .then_ignore(just(Token::RParen))
            .map(|patterns| Pat::Tuple(patterns));

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

        cons.or(record)
            .or(lower_ident().map(|ident| Pat::Var(ident)))
            .or(just(Token::Wildcard).map(|_| Pat::Wildcard))
            .or(lit().map(Pat::Lit))
            .or(list)
            .or(tuple)
            .or(unit().map(|_| Pat::Unit))
            .map_with(|kind, e| LPat::new(kind, e.span()))
            .boxed()
    })
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
        Token::String(s) => Lit::String(s),
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

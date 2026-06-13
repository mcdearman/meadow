use crate::{
    ast::*,
    intern::InternedString,
    lexer::Lexer,
    pipeline::InputMode,
    span::{Located, Span},
    token::{LToken, Token},
};
use chumsky::{
    IterParser, Parser,
    error::Rich,
    extra,
    input::{Input, Stream, ValueInput},
    pratt::{infix, left, prefix},
    primitive::*,
    recursive::recursive,
    select,
};
use itertools::Either;

#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    Prog(Option<Prog>),
    Interactive(Option<Either<LDecl, LExpr>>),
}

pub fn parse<'src>(
    lexer: Lexer<'src>,
    input_mode: InputMode,
) -> (ParseResult, Vec<Rich<'src, Token, Span>>) {
    let eof_span = lexer
        .clone()
        .last()
        .map(|(_, span)| span)
        .unwrap_or_default();

    let stream =
        Stream::from_iter(lexer).map(eof_span.extend(Span::new(0, 0)), |(t, s): (_, _)| (t, s));

    match input_mode {
        InputMode::File(name) => {
            let (res, errors) = prog(&InternedString::from(name))
                .parse(stream)
                .into_output_errors();
            (ParseResult::Prog(res), errors)
        }
        InputMode::Interactive => {
            let (res, errors) = interactive().parse(stream).into_output_errors();
            (ParseResult::Interactive(res), errors)
        }
    }
}

fn interactive<'tokens, I>()
-> impl Parser<'tokens, I, Either<LDecl, LExpr>, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    decl().map(Either::Left).or(expr().map(Either::Right))
}

fn prog<'tokens, I>(
    name: &InternedString,
) -> impl Parser<'tokens, I, Prog, extra::Err<Rich<'tokens, Token, Span>>> + Clone
where
    I: ValueInput<'tokens, Token = Token, Span = Span>,
{
    decl()
        .repeated()
        .at_least(1)
        .collect()
        .map_with(|decls, e| Located::new(Module { name: *name, decls }, e.span()))
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

    bind_decl.map_with(|bind, e| LDecl::new(Decl::Bind(bind), e.span()))
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

        let tuple_or_paren = expr
            .clone()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LParen), just(Token::RParen))
            .map_with(|mut es: Vec<LExpr>, e| {
                let val = if es.len() == 1 {
                    *es.remove(0).value
                } else {
                    Expr::Tuple(es)
                };
                Located::new(val, e.span())
            });

        let list_expr = expr
            .clone()
            .separated_by(just(Token::Comma))
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBrack), just(Token::RBrack))
            .map_with(|es, e| Located::new(Expr::List(es), e.span()));

        let bind = {
            let pat_bind = pat()
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|(p, e)| Bind::Pat(p, e));

            let fun_bind = just(Token::Fun)
                .ignore_then(lower_ident())
                .then(lower_ident().repeated().at_least(1).collect::<Vec<_>>())
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|((name, args), body)| Bind::Fun(name, args, body));

            fun_bind.or(pat_bind)
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
            .then_ignore(just(Token::LArrow))
            .then(expr.clone())
            .map(|(params, body)| Expr::Lam(params, body))
            .map_with(|e, ex| Located::new(e, ex.span()));

        let case_branch = just(Token::Bar)
            .ignore_then(pat())
            .then_ignore(just(Token::RArrow))
            .then(expr.clone());

        let match_expr = just(Token::Match)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::With))
            .then(case_branch.repeated().at_least(1).collect::<Vec<_>>())
            .map(|(scrutinee, branches)| Expr::Match(scrutinee, branches))
            .map_with(|e, ex| Located::new(e, ex.span()));

        let atom = choice((
            let_expr,
            if_expr,
            lam_expr,
            match_expr,
            unit_expr,
            tuple_or_paren,
            list_expr,
            lit_expr,
            var_expr,
        ));

        let app = atom
            .clone()
            .then(atom.repeated().collect::<Vec<_>>())
            .map_with(|(f, args), e| {
                if args.is_empty() {
                    f
                } else {
                    args.into_iter().fold(f, |acc, arg| {
                        let span = e.span();
                        Located::new(Expr::App(acc, vec![arg]), span)
                    })
                }
            });

        app.clone().pratt((
            prefix(1, just(Token::Minus), |op: Token, exp: Located<Expr>, e| {
                Located::new(
                    Expr::UnOp(Located::new(UnOp::from(op), e.span()), exp),
                    e.span(),
                )
            }),
            // infix ops
            // (
            //     2,
            //     just(Token::Plus),
            //     |op: Token, left: Located<Expr>, right: Located<Expr>, e| {
            //         Located::new(
            //             Expr::BinOp(Located::new(BinOp::from(op), e.span()), left, right),
            //             e.span(),
            //         )
            //     },
            // ),
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
            .then(pat.clone().repeated().collect())
            .map(|(name, args)| Pat::Cons(name, args));

        cons.or(lower_ident().map(|ident| Pat::Var(ident)))
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

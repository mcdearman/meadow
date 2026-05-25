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
    input::ValueInput,
    pratt::{infix, left, prefix},
    primitive::*,
    recursive::recursive,
    select,
};

pub fn parse<'src>(
    lexer: Lexer<'src>,
    input_mode: InputMode,
) -> (Option<Prog>, Vec<Rich<'src, LToken, Span>>) {
    todo!()
}

fn expr_parser<'tokens>() -> impl Parser<'tokens, ParserInput<'tokens>, LExpr, Extra<'tokens>> {
    recursive(|expr| {
        // ── Atoms ─────────────────────────────────────────────────────────

        let lit_expr = located(lit().map(Expr::Lit));
        let var_expr = located(ident().map(Expr::Var));
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
                    es.remove(0).value
                } else {
                    Expr::Tuple(es)
                };
                Located::new(val, e.span())
            });

        let list_expr = expr
            .clone()
            .separated_by(just(Token::Comma))
            .collect::<Vec<_>>()
            .delimited_by(just(Token::LBracket), just(Token::RBracket))
            .map_with(|es, e| Located::new(Expr::List(es), e.span()));

        // ── Let ───────────────────────────────────────────────────────────

        // bind: either `pat = expr` or `ident args = expr`
        let bind = {
            let pat_bind = pat_parser()
                .then_ignore(just(Token::Eq))
                .then(expr.clone())
                .map(|(p, e)| Bind::Pat(p, e));

            let fun_bind = ident()
                .then(ident().repeated().at_least(1).collect::<Vec<_>>())
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

        // ── If ────────────────────────────────────────────────────────────

        let if_expr = just(Token::If)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Then))
            .then(expr.clone())
            .then_ignore(just(Token::Else))
            .then(expr.clone())
            .map(|((cond, then), else_)| Expr::If(cond, then, else_))
            .map_with(|e, ex| Located::new(e, ex.span()));

        // ── Lambda ────────────────────────────────────────────────────────

        let lam_expr = just(Token::Backslash)
            .ignore_then(pat().repeated().at_least(1).collect::<Vec<_>>())
            .then_ignore(just(Token::LArrow))
            .then(expr.clone())
            .map(|(params, body)| Expr::Lam(params, body))
            .map_with(|e, ex| Located::new(e, ex.span()));

        // ── Case ──────────────────────────────────────────────────────────

        let case_branch = pat().then_ignore(just(Token::LArrow)).then(expr.clone());

        let case_expr = just(Token::Case)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::Of))
            .then(
                just(Token::Bar)
                    .or_not()
                    .ignore_then(case_branch.clone())
                    .then(
                        just(Token::Bar)
                            .ignore_then(case_branch)
                            .repeated()
                            .collect::<Vec<_>>(),
                    )
                    .map(|(first, mut rest)| {
                        rest.insert(0, first);
                        rest
                    }),
            )
            .map(|(scrutinee, branches)| Expr::Case(scrutinee, branches))
            .map_with(|e, ex| Located::new(e, ex.span()));

        // ── Atom (used as pratt leaf & function/arg position) ─────────────

        let atom = choice((
            let_expr,
            if_expr,
            lam_expr,
            case_expr,
            unit_expr,
            tuple_or_paren,
            list_expr,
            lit_expr,
            var_expr,
        ));

        // ── Function application (left-associative, higher than any op) ───
        //
        // `f a b c` → App(App(App(f, a), b), c)
        // We parse a non-empty sequence of atoms and fold left.

        let app = atom
            .clone()
            .then(atom.repeated().collect::<Vec<_>>())
            .map_with(|(f, args), e| {
                if args.is_empty() {
                    f
                } else {
                    // fold: App(App(f, a), b) ...
                    args.into_iter().fold(f, |acc, arg| {
                        let span = e.span();
                        Located::new(Expr::App(acc, vec![arg]), span)
                    })
                }
            });

        // ── Pratt operator table ──────────────────────────────────────────
        //
        // Precedence levels (higher = tighter):
        //   1  ||
        //   2  &&
        //   3  == != < > <= >=
        //   4  + -
        //   5  * / %
        //   6  unary !  (prefix)

        let mk_binop = |op: &'static str| {
            move |lhs: LExpr, rhs: LExpr| -> LExpr {
                let span = SimpleSpan::new(lhs.span.start, rhs.span.end);
                Located::new(
                    Expr::App(
                        Box::new(Located::new(
                            Expr::Var(Located::new(InternedString::from(op), span)),
                            span,
                        ))
                        .into(),
                        vec![lhs, rhs],
                    ),
                    span,
                )
            }
        };

        app.clone().pratt((
            // Prefix
            prefix(6, just(Token::Bang), |_, rhs: LExpr, e: &mut dyn chumsky::inspector::Inspector<ParserInput<'tokens>, Extra<'tokens>>| {
                let span = rhs.span;
                Located::new(
                    Expr::App(
                        Located::new(
                            Expr::Var(Located::new(InternedString::from("!"), span)),
                            span,
                        ),
                        vec![rhs],
                    ),
                    span,
                )
            }),
            prefix(6, just(Token::Minus), |_, rhs: LExpr, e: &mut dyn chumsky::inspector::Inspector<ParserInput<'tokens>, Extra<'tokens>>| {
                let span = rhs.span;
                Located::new(
                    Expr::App(
                        Located::new(
                            Expr::Var(Located::new(InternedString::from("negate"), span)),
                            span,
                        ),
                        vec![rhs],
                    ),
                    span,
                )
            }),
            // Multiplicative
            infix(left(5), just(Token::Star),    mk_binop("*")),
            infix(left(5), just(Token::Slash),   mk_binop("/")),
            infix(left(5), just(Token::Percent), mk_binop("%")),
            // Additive
            infix(left(4), just(Token::Plus),  mk_binop("+")),
            infix(left(4), just(Token::Minus), mk_binop("-")),
            // Comparison
            infix(left(3), just(Token::Eq),  mk_binop("==")),
            infix(left(3), just(Token::Neq),mk_binop("!=")),
            infix(left(3), just(Token::Lt),    mk_binop("<")),
            infix(left(3), just(Token::Gt),    mk_binop(">")),
            infix(left(3), just(Token::Leq),  mk_binop("<=")),
            infix(left(3), just(Token::Geq),  mk_binop(">=")),
            // Logical
            infix(left(2), just(Token::AmpAmp),  mk_binop("&&")),
            infix(left(1), just(Token::PipePipe), mk_binop("||")),
        ))
    })
}

fn pat<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, LPat, extra::Err<Rich<'a, Token, Span>>> {
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

        lower_ident()
            .map(|ident| Pat::Var(ident))
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
-> impl Parser<'a, I, (), extra::Err<Rich<'a, Token, Span>>> {
    just(Token::LParen)
        .ignore_then(just(Token::RParen))
        .map_with(|_, _| ())
}

fn lower_ident<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> {
    select! {
        Token::LowerIdent(name) => name
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

fn upper_ident<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Ident, extra::Err<Rich<'a, Token, Span>>> {
    select! {
        Token::UpperIdent(name) => name
    }
    .map_with(|name, e| Ident::new(name, e.span()))
}

fn lit<'a, I: ValueInput<'a, Token = Token, Span = Span>>()
-> impl Parser<'a, I, Lit, extra::Err<Rich<'a, Token, Span>>> {
    select! {
        Token::Int(i) => Lit::Int(i),
        Token::String(s) => Lit::String(s),
    }
}

fn located<'tokens, P, T>(
    p: P,
) -> impl Parser<'tokens, ParserInput<'tokens>, Located<T>, Extra<'tokens>>
where
    P: Parser<'tokens, ParserInput<'tokens>, T, Extra<'tokens>>,
    T: Clone,
{
    p.map_with(|v, e| Located::new(v, e.span()))
}

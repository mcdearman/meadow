//! **A boolean is always a boolean.**
//!
//! `True` and `False` are constructors a program can name, and a comparison
//! answers a boolean without naming either, so until this pass a `Bool` at run
//! time could be either an immediate or a constructor object -- and every
//! engine had to accept both wherever it branched, compared or printed one.
//!
//! A value that is one machine word has room for one representation per type.
//! So before anything below core sees the program, the constructors become the
//! literals: `True` is `true` wherever it is built and wherever it is matched.

use crate::*;

fn which(name: &str) -> Option<bool> {
    match name.rsplit('.').next() {
        Some("True") => Some(true),
        Some("False") => Some(false),
        _ => None,
    }
}

/// The program, its booleans literals.
pub fn program(p: &Program) -> Program {
    Program {
        defs: p
            .defs
            .iter()
            .map(|d| Def {
                term: term(&d.term),
                ..d.clone()
            })
            .collect(),
        ..p.clone()
    }
}

/// One term, its booleans literals.
pub fn term(t: &Term) -> Term {
    rewrite::term(
        t,
        &mut |t| match t {
            Term::Ctor(name, _, ref args) if args.is_empty() => match which(&name) {
                Some(b) => Term::Lit(Lit::Bool(b)),
                None => t,
            },
            other => other,
        },
        &mut |p| match p {
            Pat::Ctor(name, ref subs) if subs.is_empty() => match which(&name) {
                Some(b) => Pat::Lit(Lit::Bool(b)),
                None => p,
            },
            other => other,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constructors_become_literals_wherever_they_are() {
        let x = hir::VarId(1);
        let t = Term::case(
            Term::ctor("True", vec![]),
            vec![
                (
                    Pat::Ctor("Bool.False".into(), vec![]),
                    Term::ctor("False", vec![]),
                ),
                (Pat::Var(x, unknown()), Term::Var(x)),
            ],
        );
        let out = term(&t);
        let Term::Case(s, arms, _) = out else {
            panic!()
        };
        assert_eq!(*s, Term::Lit(Lit::Bool(true)));
        assert_eq!(arms[0].0, Pat::Lit(Lit::Bool(false)));
        assert_eq!(arms[0].2, Term::Lit(Lit::Bool(false)));
        // A constructor with fields that happens to be called `True` is left
        // alone -- only `Bool`'s are nullary.
        assert_eq!(
            term(&Term::ctor("True", vec![Term::Lit(Lit::Int(1))])),
            Term::ctor("True", vec![Term::Lit(Lit::Int(1))])
        );
    }
}

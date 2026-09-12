//! **Lint: the core type checker.**
//!
//! Core is typed so that a pass over it can be *checked*, which is the whole
//! reason the types are there. An optimisation that inlines a function at the
//! wrong type, forgets to substitute a type argument, or builds a constructor
//! with the wrong field is a miscompile — a program that still runs, and runs
//! wrong, usually on an input no test has. This pass finds it at the pass that
//! introduced it, by the simple expedient of recomputing every type and
//! checking it against what the term claims.
//!
//! Named after GHC's `-dcore-lint`, and doing the same job.
//!
//! # What it checks
//!
//! Every binder is annotated and every polymorphic mention carries its type
//! arguments, so there is nothing to infer: each term has exactly one type,
//! computed bottom-up in one pass. The checks are that an application's
//! argument is what the function wanted, that both branches of an `if` and
//! every arm of a `case` agree, that a constructor is applied to fields of the
//! types its declaration gives, that a `TyApp` supplies one argument per
//! binder of the thing it instantiates, and that no name is used before it is
//! bound.
//!
//! # What it does not check
//!
//! * **Effects.** A core `Fun` carries an effect row because it shares its
//!   type representation with inference, but core has no effect discipline to
//!   preserve — every backend erases them — so two function types that differ
//!   only in their effects are the same type here.
//! * **Primitive operands.** Arity, and nothing more: a primitive's operand
//!   types are the front end's business, and several are deliberately loose
//!   (`Eq` compares anything, an integer literal may be `Int` or `BigInt`).
//! * **Effect operations.** A `perform` produces what it is annotated with.
//!   Checking it would need the operation signatures, which this crate does
//!   not have.
//! * **Anything involving [`unknown`].** It is what lowering writes when a
//!   unit already has type errors, and a second complaint about the first
//!   one's consequences helps nobody.

use crate::*;
use meadow_infer::{normalize, VariantEnv};

/// Rigid ids for an imported scheme's quantifiers are allocated up here, where
/// they cannot collide with this unit's (which are inference arena indices).
const IMPORT_BASE: u32 = 0x8000_0000;

/// Check a whole program. An empty result means it type-checks.
///
/// `imported` covers bindings this program mentions but does not define — a
/// dependency's exports, when a single unit is being checked before linking.
pub fn check(
    program: &Program,
    ctors: &VariantEnv,
    imported: &HashMap<Var, Scheme>,
) -> Vec<String> {
    let mut globals: HashMap<Var, Poly> = imported
        .iter()
        .map(|(v, s)| (*v, import(s)))
        .collect();
    for d in &program.defs {
        globals.insert(d.var, d.poly.clone());
    }

    let mut lint = Lint {
        globals,
        ctors,
        locals: Vec::new(),
        errors: Vec::new(),
        where_: InternedString::from(""),
    };
    for d in &program.defs {
        lint.where_ = d.name;
        let got = lint.synth(&d.term);
        if !poly_same(&got, &d.poly) {
            lint.say(format!(
                "definition's type is not what its body has\n  claimed: {:?}\n  found:   {:?}",
                d.poly, got
            ));
        }
    }
    lint.errors
}

/// An imported scheme as a core polytype: its numbered quantifiers become
/// rigid variables of their own.
fn import(s: &Scheme) -> Poly {
    let binders: Vec<TyVar> = s
        .quant
        .iter()
        .enumerate()
        .map(|(i, kind)| TyVar {
            id: IMPORT_BASE + i as u32,
            kind: *kind,
        })
        .collect();
    let map: HashMap<u32, Ty> = binders
        .iter()
        .enumerate()
        .map(|(i, b)| (i as u32, InferType::Var(b.id)))
        .collect();
    Poly {
        ty: subst_bound(&s.ty, &map),
        binders,
    }
}

struct Lint<'a> {
    globals: HashMap<Var, Poly>,
    ctors: &'a VariantEnv,
    /// Value bindings in scope, innermost last.
    locals: Vec<(Var, Poly)>,
    errors: Vec<String>,
    /// The definition being checked, for the message.
    where_: InternedString,
}

impl Lint<'_> {
    fn say(&mut self, msg: String) {
        let name = self.where_;
        self.errors.push(format!("in `{name}`: {msg}"));
    }

    fn lookup(&self, v: Var) -> Option<Poly> {
        self.locals
            .iter()
            .rev()
            .find(|(x, _)| *x == v)
            .map(|(_, p)| p.clone())
            .or_else(|| self.globals.get(&v).cloned())
    }

    /// The type of a term, checking it on the way.
    ///
    /// Returns a [`Poly`] rather than a type because two terms have one:
    /// a mention of a polymorphic binding, and the `TyLam` that introduces
    /// one. Everywhere else it is monomorphic, and [`Lint::synth_mono`] is
    /// what asks for that.
    fn synth(&mut self, t: &Term) -> Poly {
        match t {
            Term::Var(v) => match self.lookup(*v) {
                Some(p) => p,
                None => {
                    self.say(format!("unbound variable {v:?}"));
                    Poly::mono(unknown())
                }
            },
            Term::Lit(l) => Poly::mono(lit_ty(l)),

            Term::TyLam(binders, body) => {
                let inner = self.synth_mono(body);
                Poly {
                    binders: binders.clone(),
                    ty: inner,
                }
            }
            Term::TyApp(f, args) => {
                let p = self.synth(f);
                if p.binders.len() != args.len() {
                    // A monomorphic thing given type arguments, or the wrong
                    // number of them: the usual shape of a botched inline.
                    self.say(format!(
                        "instantiated with {} type argument(s) but binds {}",
                        args.len(),
                        p.binders.len()
                    ));
                    return Poly::mono(unknown());
                }
                Poly::mono(p.instantiate(args))
            }

            Term::Lam(v, ty, body) => {
                self.locals.push((*v, Poly::mono(ty.clone())));
                let ret = self.synth_mono(body);
                self.locals.pop();
                Poly::mono(InferType::Fun(
                    vec![ty.clone()],
                    Box::new(ret),
                    // Effects are not core's to preserve — see the module docs.
                    Box::new(unknown()),
                ))
            }
            Term::App(f, a) => {
                let fty = self.synth_mono(f);
                let aty = self.synth_mono(a);
                match fty {
                    InferType::Fun(params, ret, _) => {
                        let want = params.first().cloned().unwrap_or_else(unknown);
                        self.expect(&want, &aty, "argument");
                        Poly::mono(*ret)
                    }
                    other if is_unknown(&other) => Poly::mono(unknown()),
                    other => {
                        self.say(format!("applied something that is not a function: {other:?}"));
                        Poly::mono(unknown())
                    }
                }
            }

            Term::Let(v, poly, rhs, body) => {
                self.check_binding(poly, rhs);
                self.locals.push((*v, poly.clone()));
                let ty = self.synth_mono(body);
                self.locals.pop();
                Poly::mono(ty)
            }
            Term::LetRec(binds, body) => {
                // Every binding of the group is in scope in every right-hand
                // side, which is what makes it a group.
                for (v, poly, _) in binds {
                    self.locals.push((*v, poly.clone()));
                }
                for (_, poly, rhs) in binds {
                    self.check_binding(poly, rhs);
                }
                let ty = self.synth_mono(body);
                for _ in binds {
                    self.locals.pop();
                }
                Poly::mono(ty)
            }

            Term::If(c, a, b) => {
                let cty = self.synth_mono(c);
                self.expect(&InferType::bool(), &cty, "condition of `if`");
                let at = self.synth_mono(a);
                let bt = self.synth_mono(b);
                self.expect(&at, &bt, "branches of `if`");
                Poly::mono(at)
            }

            Term::Tuple(items) => {
                let tys = items.iter().map(|x| self.synth_mono(x)).collect();
                Poly::mono(InferType::Tuple(tys))
            }
            Term::Proj(x, i) => {
                let ty = self.synth_mono(x);
                match ty {
                    InferType::Tuple(items) => match items.get(*i) {
                        Some(t) => Poly::mono(t.clone()),
                        None => {
                            self.say(format!("projected field {i} of a {}-tuple", items.len()));
                            Poly::mono(unknown())
                        }
                    },
                    other if is_unknown(&other) => Poly::mono(unknown()),
                    other => {
                        self.say(format!("projected a field of a non-tuple: {other:?}"));
                        Poly::mono(unknown())
                    }
                }
            }
            Term::Array(items, elem) => {
                for x in items {
                    let ty = self.synth_mono(x);
                    self.expect(elem, &ty, "array element");
                }
                Poly::mono(InferType::Con(
                    InternedString::from("Array"),
                    vec![elem.clone()],
                ))
            }
            Term::Record(fields) => {
                let row = fields.iter().rev().fold(InferType::RowEmpty, |acc, (l, x)| {
                    let ty = self.synth_mono(x);
                    InferType::RowExtend(*l, Box::new(ty), Box::new(acc))
                });
                Poly::mono(InferType::Record(Box::new(row)))
            }
            Term::Sel(x, _, ty) => {
                let _ = self.synth_mono(x);
                Poly::mono(ty.clone())
            }
            Term::Extend(rec, l, v) => {
                let base = self.synth_mono(rec);
                let field = self.synth_mono(v);
                let row = match base {
                    InferType::Record(row) => *row,
                    other => other,
                };
                Poly::mono(InferType::Record(Box::new(InferType::RowExtend(
                    *l,
                    Box::new(field),
                    Box::new(row),
                ))))
            }

            Term::Ctor(name, ty, args) => {
                if let Some(fields) = self.ctor_fields(*name, ty) {
                    if fields.len() != args.len() {
                        self.say(format!(
                            "`{name}` takes {} field(s), given {}",
                            fields.len(),
                            args.len()
                        ));
                    }
                    for (want, arg) in fields.iter().zip(args) {
                        let got = self.synth_mono(arg);
                        self.expect(want, &got, &format!("field of `{name}`"));
                    }
                } else {
                    for a in args {
                        let _ = self.synth_mono(a);
                    }
                }
                Poly::mono(ty.clone())
            }

            Term::Case(scrut, arms, ty) => {
                let sty = self.synth_mono(scrut);
                for (pat, body) in arms {
                    let mark = self.locals.len();
                    self.check_pat(pat, &sty);
                    let got = self.synth_mono(body);
                    self.expect(ty, &got, "arm of `case`");
                    self.locals.truncate(mark);
                }
                Poly::mono(ty.clone())
            }

            Term::Prim(op, args, ty) => {
                if args.len() != op.arity() {
                    self.say(format!(
                        "`{op:?}` takes {} operand(s), given {}",
                        op.arity(),
                        args.len()
                    ));
                }
                for a in args {
                    let _ = self.synth_mono(a);
                }
                Poly::mono(ty.clone())
            }
            Term::Perform(_, _, arg, ty) => {
                let _ = self.synth_mono(arg);
                Poly::mono(ty.clone())
            }
            Term::Handle {
                body,
                clauses,
                ret,
                ty,
            } => {
                let bty = self.synth_mono(body);
                for c in clauses {
                    let mark = self.locals.len();
                    self.locals.push((c.param, Poly::mono(c.param_ty.clone())));
                    self.locals
                        .push((c.resume, Poly::mono(c.resume_ty.clone())));
                    let got = self.synth_mono(&c.body);
                    self.expect(ty, &got, "handler clause");
                    self.locals.truncate(mark);
                }
                match ret {
                    Some((v, vty, rbody)) => {
                        self.expect(vty, &bty, "`return` clause's parameter");
                        self.locals.push((*v, Poly::mono(vty.clone())));
                        let got = self.synth_mono(rbody);
                        self.expect(ty, &got, "`return` clause");
                        self.locals.pop();
                    }
                    // No `return` clause is the identity, so the body itself
                    // has to produce the handler's type.
                    None => self.expect(ty, &bty, "body of `handle`"),
                }
                Poly::mono(ty.clone())
            }

            Term::Error => Poly::mono(unknown()),
        }
    }

    /// The type of a term that must not be polymorphic.
    fn synth_mono(&mut self, t: &Term) -> Ty {
        let p = self.synth(t);
        if !p.binders.is_empty() {
            self.say(
                "a polymorphic binding is used without type arguments (a missing `TyApp`)"
                    .to_string(),
            );
        }
        p.ty
    }

    /// A binding's right-hand side against the type the binder claims.
    fn check_binding(&mut self, poly: &Poly, rhs: &Term) {
        let got = self.synth(rhs);
        if !poly_same(&got, poly) {
            self.say(format!(
                "binding's type is not what its right-hand side has\n  claimed: {:?}\n  found:   {:?}",
                poly, got
            ));
        }
    }

    /// Bind a pattern's variables, checking it against what it matches.
    fn check_pat(&mut self, pat: &Pat, scrut: &Ty) {
        match pat {
            Pat::Wild => {}
            Pat::Var(v, ty) => {
                self.expect(ty, scrut, "pattern variable");
                self.locals.push((*v, Poly::mono(ty.clone())));
            }
            Pat::As(v, ty, sub) => {
                self.expect(ty, scrut, "`as` pattern");
                self.locals.push((*v, Poly::mono(ty.clone())));
                self.check_pat(sub, scrut);
            }
            Pat::Lit(_) => {}
            Pat::Tuple(ps) => match scrut {
                InferType::Tuple(items) if items.len() == ps.len() => {
                    for (p, t) in ps.iter().zip(items) {
                        self.check_pat(p, t);
                    }
                }
                other if is_unknown(other) => self.unknown_pats(ps),
                _ => {
                    self.say("tuple pattern on a non-tuple".to_string());
                    self.unknown_pats(ps);
                }
            },
            Pat::Array(ps) => {
                let elem = match scrut {
                    InferType::Con(n, args) if &**n == "Array" && args.len() == 1 => {
                        args[0].clone()
                    }
                    _ => unknown(),
                };
                for p in ps {
                    self.check_pat(p, &elem);
                }
            }
            Pat::Ctor(name, ps) => match self.ctor_fields(*name, scrut) {
                Some(fields) => {
                    if fields.len() != ps.len() {
                        self.say(format!(
                            "pattern `{name}` binds {} field(s) of {}",
                            ps.len(),
                            fields.len()
                        ));
                    }
                    for (p, t) in ps.iter().zip(&fields) {
                        self.check_pat(p, t);
                    }
                }
                None => self.unknown_pats(ps),
            },
            Pat::Record(fields) => {
                for (label, p) in fields {
                    let ty = row_field(scrut, *label).unwrap_or_else(unknown);
                    self.check_pat(p, &ty);
                    let _ = label;
                }
            }
        }
    }

    fn unknown_pats(&mut self, ps: &[Pat]) {
        let u = unknown();
        for p in ps {
            self.check_pat(p, &u);
        }
    }

    /// A constructor's field types, at the type it builds.
    ///
    /// `None` when the constructor is not one this program declares — a
    /// builtin the front end desugars to, or a unit that failed to compile.
    fn ctor_fields(&mut self, name: InternedString, at: &Ty) -> Option<Vec<Ty>> {
        let (owner, args) = match at {
            InferType::Con(n, args) => (*n, args.clone()),
            _ => return None,
        };
        let sig = self
            .ctors
            .get(&owner)?
            .iter()
            .find(|v| v.name == name)?;
        Some(
            sig.fields
                .iter()
                .map(|f| meadow_infer::subst_bound(f, &args))
                .collect(),
        )
    }

    fn expect(&mut self, want: &Ty, got: &Ty, what: &str) {
        if !same(want, got) {
            self.say(format!(
                "{what} has the wrong type\n  wanted: {want:?}\n  got:    {got:?}"
            ));
        }
    }
}

fn lit_ty(l: &Lit) -> Ty {
    match l {
        Lit::Int(_) => InferType::int(),
        Lit::BigInt(_) => InferType::con("BigInt"),
        Lit::Float(_) => InferType::float(),
        Lit::Str(_) => InferType::string(),
        Lit::Char(_) => InferType::char(),
        Lit::Bool(_) => InferType::bool(),
        Lit::Unit => InferType::unit(),
    }
}

/// A record row's field type, if the row says.
fn row_field(ty: &Ty, label: InternedString) -> Option<Ty> {
    let mut cur = match ty {
        InferType::Record(row) => &**row,
        other => other,
    };
    loop {
        match cur {
            InferType::RowExtend(l, f, rest) => {
                if *l == label {
                    return Some((**f).clone());
                }
                cur = rest;
            }
            _ => return None,
        }
    }
}

/// Are two polytypes the same? Their binders have to line up in kind, and
/// their bodies have to agree once one's binders are renamed to the other's.
fn poly_same(a: &Poly, b: &Poly) -> bool {
    if a.binders.len() != b.binders.len() {
        return false;
    }
    let map: HashMap<u32, Ty> = a
        .binders
        .iter()
        .zip(&b.binders)
        .map(|(x, y)| (x.id, InferType::Var(y.id)))
        .collect();
    same(&subst_rigid(&a.ty, &map), &b.ty)
}

/// Type equality, as core means it: up to the order of a row's labels, with
/// effects ignored and [`unknown`] matching anything. See the module docs.
fn same(a: &Ty, b: &Ty) -> bool {
    if is_unknown(a) || is_unknown(b) {
        return true;
    }
    match (a, b) {
        (InferType::Var(x), InferType::Var(y)) => x == y,
        (InferType::Bound(x), InferType::Bound(y)) => x == y,
        (InferType::RowEmpty, InferType::RowEmpty) => true,
        (InferType::Con(n1, a1), InferType::Con(n2, a2)) => {
            n1 == n2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| same(x, y))
        }
        // The effect row is the third component, and it is not compared.
        (InferType::Fun(p1, r1, _), InferType::Fun(p2, r2, _)) => {
            p1.len() == p2.len()
                && p1.iter().zip(p2).all(|(x, y)| same(x, y))
                && same(r1, r2)
        }
        (InferType::Tuple(x), InferType::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (InferType::Record(x), InferType::Record(y)) => same(x, y),
        (InferType::RowExtend(..), InferType::RowExtend(..)) => {
            // Rows are sets; compare them in one order.
            match (normalize(a), normalize(b)) {
                (InferType::RowExtend(l1, f1, r1), InferType::RowExtend(l2, f2, r2)) => {
                    l1 == l2 && same(&f1, &f2) && same(&r1, &r2)
                }
                (x, y) => x == y,
            }
        }
        _ => false,
    }
}

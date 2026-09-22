//! **Hygiene**: keeping a template's locals apart from the call site's.
//!
//! A macro that writes `let tmp = …` must not capture a `tmp` the caller passed
//! in, and must not be captured by one the caller defines. Meadow takes Rust's
//! **mixed-site** position: local bindings are hygienic, and items -- anything
//! declared at the top level, and every type, constructor and field name -- are
//! not.
//!
//! The mechanism is a **mark**. Every identifier a template writes is renamed
//! `name#3`, where the number is the expansion's. A `#` cannot be lexed into an
//! identifier, so no source can spell a marked name, and two expansions never
//! share one. The rest follows:
//!
//! * a template's `let tmp#3 = …` binds a name the caller cannot write, so the
//!   caller's `tmp` is a different variable;
//! * what came from the *call* is spliced in unmarked, so `$x` still means the
//!   caller's `x`;
//! * a marked name that nothing bound -- `push#3`, where the template meant the
//!   ordinary `push` -- is not found, and the resolver looks again without the
//!   mark ([`meadow_rename`] does this). That is what makes items unhygienic.
//!
//! What marking must *not* touch is every lowercase name that is not a
//! variable: a record label, a module member, a type variable, the name of a
//! declaration. Those are matched by name against something outside the
//! expansion, so a mark would break them. They are marked on the way out and
//! stripped here, once the tokens have been parsed and it is clear which is
//! which -- a label and a variable are the same token, but different slots.

use meadow_ast as ast;
use meadow_lexer::Token;

/// Mark a token a template wrote, if it is the sort of name that can be a
/// local. Uppercase names are types and constructors, which are never
/// hygienic, so they are left as they are.
pub fn mark(t: &Token, id: u32) -> Token {
    match t {
        Token::LowerIdent(name) => Token::LowerIdent(ast::hygiene::mark(*name, id)),
        other => other.clone(),
    }
}

/// Take the marks off every name in `decls` that is not a local variable.
///
/// Run on what one expansion produced, after it is parsed.
pub fn strip_items(decls: &mut [ast::LDecl]) {
    for d in decls {
        decl(d);
    }
}

/// The same, for an expansion that produced an expression or a pattern: there
/// is no declaration, so only the slots inside it need stripping.
pub fn strip_in_expr(e: &mut ast::LExpr) {
    expr(e);
}

pub fn strip_in_pat(p: &mut ast::LPat) {
    pat(p);
}

fn unmark(id: &mut ast::Ident) {
    let name = *id.value();
    let bare = ast::hygiene::strip(name);
    if bare != name {
        *id = ast::Ident::new(bare, id.span);
    }
}

fn bound(b: &mut ast::Bound) {
    unmark(&mut b.tr);
    b.tys.iter_mut().for_each(ty);
}

fn decl(d: &mut ast::LDecl) {
    match &mut *d.value {
        // A top-level binding is an item: its *name* is not hygienic, though
        // its parameters and body are.
        ast::Decl::Bind(b) => {
            match b {
                ast::Bind::Fun(name, _, _, _) => unmark(name),
                ast::Bind::Pat(p, _) => item_pat(p),
            }
            bind(b);
        }
        ast::Decl::Sig(name, t, bounds) => {
            unmark(name);
            ty(t);
            bounds.iter_mut().for_each(bound);
        }
        ast::Decl::Trait(td) => {
            unmark(&mut td.name);
            td.params.iter_mut().for_each(unmark);
            td.supers.iter_mut().for_each(bound);
            for (name, params) in &mut td.assocs {
                unmark(name);
                params.iter_mut().for_each(unmark);
            }
            for (name, t) in &mut td.sigs {
                unmark(name);
                ty(t);
            }
            for b in &mut td.defaults {
                if let ast::Bind::Fun(name, _, _, _) = b {
                    unmark(name);
                }
                bind(b);
            }
        }
        ast::Decl::Impl(id) => {
            unmark(&mut id.tr);
            id.tys.iter_mut().for_each(ty);
            id.context.iter_mut().for_each(bound);
            for (name, at, is) in &mut id.assocs {
                unmark(name);
                at.iter_mut().for_each(ty);
                ty(is);
            }
            for b in &mut id.methods {
                match b {
                    ast::Bind::Fun(name, _, _, _) => unmark(name),
                    ast::Bind::Pat(p, _) => item_pat(p),
                }
                bind(b);
            }
        }
        ast::Decl::Data(dd) => {
            unmark(&mut dd.name);
            for p in &mut dd.params {
                unmark(p);
            }
            for v in &mut dd.variants {
                match &mut v.fields {
                    ast::VariantFields::Positional(ts) => ts.iter_mut().for_each(ty),
                    ast::VariantFields::Named(fs) => fs.iter_mut().for_each(field),
                }
            }
        }
        ast::Decl::Record(rd) => {
            unmark(&mut rd.name);
            for p in &mut rd.params {
                unmark(p);
            }
            rd.fields.iter_mut().for_each(field);
        }
        ast::Decl::Effect(ed) => {
            unmark(&mut ed.name);
            for p in &mut ed.params {
                unmark(p);
            }
            ed.ops.iter_mut().for_each(field);
        }
        ast::Decl::TypeAlias(ad) => {
            unmark(&mut ad.name);
            for p in &mut ad.params {
                unmark(p);
            }
            ty(&mut ad.ty);
        }
        ast::Decl::Use(u) => {
            for n in &mut u.path {
                unmark(n);
            }
            for n in &mut u.names {
                unmark(n);
            }
            if let Some(a) = &mut u.alias {
                unmark(a);
            }
        }
        ast::Decl::Attributed(_, inner) => decl(inner),
        ast::Decl::Mod(n) => unmark(n),
        ast::Decl::Fixity(_, _, ops) => ops.iter_mut().for_each(unmark),
        // Gone by the time this runs.
        ast::Decl::MacCall(_) | ast::Decl::Macro(_) => {}
    }
}

/// The names a top-level `def` pattern binds, which are items.
fn item_pat(p: &mut ast::LPat) {
    match &mut *p.value {
        ast::Pat::Var(n) => unmark(n),
        ast::Pat::Ann(inner, t) => {
            item_pat(inner);
            ty(t);
        }
        ast::Pat::Tuple(ps) | ast::Pat::Array(ps) | ast::Pat::Vector(ps) | ast::Pat::List(ps) => {
            ps.iter_mut().for_each(item_pat)
        }
        ast::Pat::As(n, inner) => {
            unmark(n);
            item_pat(inner);
        }
        _ => pat(p),
    }
}

fn field(f: &mut ast::Field) {
    unmark(&mut f.name);
    ty(&mut f.ty);
}

/// Every name in a type is a type's: a variable, a constructor, a label. None
/// of them is a local, so none of them keeps a mark.
fn ty(t: &mut ast::LType) {
    match &mut *t.value {
        ast::TypeExpr::Var(n) => unmark(n),
        ast::TypeExpr::Con(n, args) => {
            unmark(n);
            args.iter_mut().for_each(ty);
        }
        ast::TypeExpr::Fun(args, ret, eff) => {
            args.iter_mut().for_each(ty);
            ty(ret);
            if let Some(row) = eff {
                for (label, args) in &mut row.labels {
                    unmark(label);
                    args.iter_mut().for_each(ty);
                }
                if let Some(tail) = &mut row.tail {
                    unmark(tail);
                }
            }
        }
        ast::TypeExpr::Tuple(ts) => ts.iter_mut().for_each(ty),
        ast::TypeExpr::Vector(t) | ast::TypeExpr::List(t) => ty(t),
        ast::TypeExpr::Record(fields, tail) => {
            for (label, t) in fields {
                unmark(label);
                ty(t);
            }
            if let Some(tail) = tail {
                unmark(tail);
            }
        }
    }
}

fn bind(b: &mut ast::Bind) {
    match b {
        ast::Bind::Pat(p, e) => {
            pat(p);
            expr(e);
        }
        ast::Bind::Fun(_, params, ret, body) => {
            params.iter_mut().for_each(pat);
            if let Some(t) = ret {
                ty(t);
            }
            expr(body);
        }
    }
}

fn pat(p: &mut ast::LPat) {
    match &mut *p.value {
        // A record pattern's labels name the record's fields; only what each
        // one binds is a variable.
        ast::Pat::Record(fields, _) => {
            for (label, inner) in fields {
                unmark(label);
                pat(inner);
            }
        }
        ast::Pat::Ann(inner, t) => {
            pat(inner);
            ty(t);
        }
        ast::Pat::As(_, inner) => pat(inner),
        ast::Pat::Cons(_, ps)
        | ast::Pat::QualCons(_, _, ps)
        | ast::Pat::Tuple(ps)
        | ast::Pat::Array(ps)
        | ast::Pat::Vector(ps)
        | ast::Pat::List(ps) => ps.iter_mut().for_each(pat),
        ast::Pat::Wildcard
        | ast::Pat::Var(_)
        | ast::Pat::Lit(_)
        | ast::Pat::Unit
        | ast::Pat::MacCall(_) => {}
    }
}

fn expr(e: &mut ast::LExpr) {
    match &mut *e.value {
        // `Mod.name` -- the name is the module's, not a local.
        ast::Expr::Qual(_, name) => unmark(name),
        // `e.label` -- the label is the record's.
        ast::Expr::Field(o, label) => {
            expr(o);
            unmark(label);
        }
        ast::Expr::Record(fields, base) => {
            for (label, v) in fields {
                unmark(label);
                expr(v);
            }
            if let Some(b) = base {
                expr(b);
            }
        }
        ast::Expr::Update(base, fields) => {
            expr(base);
            for (label, v) in fields {
                unmark(label);
                expr(v);
            }
        }
        ast::Expr::Handle(body, arms, ret) => {
            expr(body);
            for arm in arms {
                // The operation is the effect's, but what it binds is local.
                unmark(&mut arm.op);
                pat(&mut arm.param);
                expr(&mut arm.body);
            }
            if let Some((p, e)) = ret {
                pat(p);
                expr(e);
            }
        }
        ast::Expr::Lam(ps, body) => {
            ps.iter_mut().for_each(pat);
            expr(body);
        }
        ast::Expr::App(f, args) => {
            expr(f);
            args.iter_mut().for_each(expr);
        }
        ast::Expr::Let(binds, body) => {
            binds.iter_mut().for_each(bind);
            expr(body);
        }
        ast::Expr::If(c, t, f) => {
            expr(c);
            expr(t);
            expr(f);
        }
        ast::Expr::Match(scrutinee, arms) => {
            expr(scrutinee);
            for (p, guard, body) in arms {
                pat(p);
                if let Some(g) = guard {
                    expr(g);
                }
                expr(body);
            }
        }
        ast::Expr::UnOp(_, x) => expr(x),
        ast::Expr::BinOp(_, l, r) => {
            expr(l);
            expr(r);
        }
        ast::Expr::Infix(first, rest) => {
            expr(first);
            rest.iter_mut().for_each(|(_, x)| expr(x));
        }
        ast::Expr::Interp(_, holes) => holes.iter_mut().for_each(|(x, _)| expr(x)),
        ast::Expr::Tuple(xs)
        | ast::Expr::Array(xs)
        | ast::Expr::List(xs)
        | ast::Expr::Cons(_, xs) => xs.iter_mut().for_each(expr),
        // A bare name is a variable, and keeps its mark: whether it is a local
        // or an item is the resolver's to decide, by looking.
        ast::Expr::Var(_)
        | ast::Expr::Lit(_)
        | ast::Expr::Unit
        | ast::Expr::Hole
        | ast::Expr::MacCall(_) => {}
    }
}

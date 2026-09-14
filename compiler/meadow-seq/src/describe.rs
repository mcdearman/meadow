//! **Every value's descriptor, wherever the value is.**
//!
//! A name whose representation is [`Rep::Var`] holds a value only its
//! descriptor explains, so anywhere that name is in an environment that can
//! collect -- which is anywhere but the moment control is handed over -- the
//! descriptor has to be there too. Lowering passes descriptors where types are
//! applied and binds them where types are abstracted, and its liveness knows
//! nothing about them; this pass adds each one to the environments that lack
//! it, the way a closure conversion adds a captured variable.
//!
//! Within one abstraction a type variable's descriptor has one name, so the
//! additions are by name, and they only ever grow an environment:
//!
//! * a block whose values need a descriptor it does not bind takes it as one
//!   more parameter, at the end, and whatever enters it passes it:
//!   a `substitute` selects it, a `new` captures it (placed after the other
//!   captures, before the method's arguments), a `jump` brings it along;
//! * the continuations of a `let`, `new`, `extern` or `switch` share the
//!   environment they continue, so they gain what it gained.
//!
//! A `substitute` that hands control straight over -- to a `jump` or an
//! `invoke` -- is left alone, apart from a jump target's additions: nothing
//! collects between it and the transfer, and its shape is the target's.
//!
//! A block reached from more than one place gains the same parameters at every
//! entry, so blocks are settled to a fixpoint first: a lifted block's needs
//! reach whoever jumps to it, which may be itself.

use crate::{Block, Def, Label, NO_DESC, Name, Rep, Statement, VarId};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Give every environment the descriptors its values need. Returns the blocks
/// that needed descriptors nothing could pass them: a definition's, when
/// lowering described a value by a variable no abstraction around it binds.
pub(crate) fn close(
    defs: &mut [Def],
    reps: &HashMap<Name, Rep>,
    descs: &HashSet<Name>,
) -> HashMap<Label, Vec<Name>> {
    let mut closer = Closer {
        reps,
        descs,
        extra: HashMap::new(),
        memo: RefCell::new(HashMap::new()),
        recording: false,
    };
    loop {
        let mut changed = false;
        for d in defs.iter() {
            let m = closer.missing(&d.block);
            if closer
                .extra
                .get(&d.label)
                .map_or(!m.is_empty(), |e| *e != m)
            {
                closer.extra.insert(d.label, m);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Settled: once more, remembering every block's additions, and then write
    // them in.
    closer.recording = true;
    for d in defs.iter() {
        closer.missing(&d.block);
    }
    for d in defs.iter_mut() {
        let old = d.block.params.clone();
        let add = closer.extra.get(&d.label).cloned().unwrap_or_default();
        d.block.params.extend(add);
        let new = d.block.params.clone();
        closer.rewrite(&mut d.block.body, &old, &new);
    }
    closer.extra.retain(|_, e| !e.is_empty());
    closer.extra
}

struct Closer<'a> {
    reps: &'a HashMap<Name, Rep>,
    descs: &'a HashSet<Name>,
    /// What each labelled block has gained.
    extra: HashMap<Label, Vec<Name>>,
    /// What each block has gained, by address, once settled -- recorded
    /// while `recording`.
    memo: RefCell<HashMap<usize, Vec<Name>>>,
    recording: bool,
}

fn address(b: &Block) -> usize {
    b as *const Block as usize
}

/// The transfer a block's body is, if it is nothing else.
pub(crate) fn transfer(s: &Statement) -> Option<&Statement> {
    match s {
        Statement::Mark(_, inner) => transfer(inner),
        Statement::Jump(_) | Statement::Invoke(..) => Some(s),
        _ => None,
    }
}

impl Closer<'_> {
    /// The descriptor `n`'s value needs, if any.
    fn desc_of(&self, n: Name) -> Option<Name> {
        match self.reps.get(&n) {
            Some(Rep::Var(d)) if *d != NO_DESC => Some(VarId(*d)),
            _ => None,
        }
    }

    /// The descriptors block `b` needs and does not bind.
    fn missing(&self, b: &Block) -> Vec<Name> {
        let mut need = BTreeSet::new();
        self.need(&b.body, &mut need);
        need.extend(b.params.iter().filter_map(|p| self.desc_of(*p)));
        for p in &b.params {
            need.remove(p);
        }
        let need: Vec<Name> = need.into_iter().collect();
        if self.recording {
            self.memo.borrow_mut().insert(address(b), need.clone());
        }
        need
    }

    /// The descriptors `s` needs in the environment it starts in.
    fn need(&self, s: &Statement, out: &mut BTreeSet<Name>) {
        match s {
            Statement::Mark(_, inner) => self.need(inner, out),
            Statement::Error(_) | Statement::Invoke(..) => {}
            Statement::Jump(l) => out.extend(self.extra.get(l).into_iter().flatten()),
            Statement::Substitute(sel, b) => {
                out.extend(sel.iter().filter(|n| self.descs.contains(n)));
                match transfer(&b.body) {
                    Some(Statement::Jump(l)) => out.extend(self.extra.get(l).into_iter().flatten()),
                    Some(_) => {}
                    None => out.extend(self.missing(b)),
                }
            }
            Statement::Let { name, rest, .. } => self.binding(*name, rest, out),
            Statement::New {
                name,
                methods,
                rest,
                ..
            } => {
                for m in methods {
                    out.extend(self.missing(m));
                }
                self.binding(*name, rest, out);
            }
            Statement::Extern { blocks, .. } => {
                for b in blocks {
                    out.extend(self.missing(b));
                }
            }
            Statement::Switch { arms, default, .. } => {
                for (_, b) in arms {
                    out.extend(self.missing(b));
                }
                out.extend(self.missing(default));
            }
        }
    }

    /// `name` is bound, and `rest` runs.
    fn binding(&self, name: Name, rest: &Statement, out: &mut BTreeSet<Name>) {
        let mut inner = BTreeSet::new();
        self.need(rest, &mut inner);
        inner.extend(self.desc_of(name));
        inner.remove(&name);
        out.extend(inner);
    }

    /// Write the additions into `s`, whose environment was `old` and is now
    /// `new`.
    fn rewrite(&self, s: &mut Statement, old: &[Name], new: &[Name]) {
        match s {
            Statement::Mark(_, inner) => self.rewrite(inner, old, new),
            Statement::Error(_) | Statement::Invoke(..) | Statement::Jump(_) => {}
            Statement::Substitute(sel, b) => match transfer(&b.body) {
                Some(Statement::Jump(l)) => {
                    let add = self.extra.get(l).cloned().unwrap_or_default();
                    sel.extend(add.iter().copied());
                    b.params.extend(add);
                }
                Some(_) => {}
                None => {
                    let add = self.memo.borrow()[&address(b)].clone();
                    sel.extend(add.iter().copied());
                    let before = b.params.clone();
                    b.params.extend(add);
                    let after = b.params.clone();
                    self.rewrite(&mut b.body, &before, &after);
                }
            },
            Statement::Let { name, rest, .. } => {
                let (old, new) = (prepend(*name, old), prepend(*name, new));
                self.rewrite(rest, &old, &new);
            }
            Statement::New {
                name,
                captures,
                methods,
                rest,
            } => {
                let mut add = BTreeSet::new();
                for m in methods.iter() {
                    add.extend(self.memo.borrow()[&address(m)].iter().copied());
                }
                let add: Vec<Name> = add.into_iter().collect();
                let c = captures.len();
                captures.extend(add.iter().copied());
                for m in methods.iter_mut() {
                    let before = m.params.clone();
                    let mut params = before[..c].to_vec();
                    params.extend(add.iter().copied());
                    params.extend_from_slice(&before[c..]);
                    m.params = params;
                    let after = m.params.clone();
                    self.rewrite(&mut m.body, &before, &after);
                }
                let (old, new) = (prepend(*name, old), prepend(*name, new));
                self.rewrite(rest, &old, &new);
            }
            Statement::Extern { blocks, .. } => {
                for b in blocks {
                    self.continuation(b, old, new);
                }
            }
            Statement::Switch { arms, default, .. } => {
                for (_, b) in arms {
                    self.continuation(b, old, new);
                }
                self.continuation(default, old, new);
            }
        }
    }

    /// A block continuing an environment that was `old` and is now `new`: what
    /// it binds, then that environment.
    fn continuation(&self, b: &mut Block, old: &[Name], new: &[Name]) {
        if old == new {
            let params = b.params.clone();
            return self.rewrite(&mut b.body, &params, &params);
        }
        let bound = b.params.len() - old.len();
        debug_assert_eq!(
            &b.params[bound..],
            old,
            "a continuation whose parameters are not the environment it continues"
        );
        let before = b.params.clone();
        let mut params = before[..bound].to_vec();
        params.extend_from_slice(new);
        b.params = params;
        let after = b.params.clone();
        self.rewrite(&mut b.body, &before, &after);
    }
}

fn prepend(n: Name, env: &[Name]) -> Vec<Name> {
    let mut out = Vec::with_capacity(env.len() + 1);
    out.push(n);
    out.extend_from_slice(env);
    out
}

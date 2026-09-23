//! **Linearization**: AxCut with every share and every erase written down.
//!
//! In the AxCut paper (see `docs/AOT.md`) the environment is linear: every
//! name is used exactly once, a `substitute` that names a variable _n_ times
//! shares it _n_ − 1 times and one that leaves it out erases it, and every
//! other statement consumes what it names. Memory is then managed by the
//! program itself, with a reference count per block and no collector.
//!
//! `meadow_seq`'s AxCut is not linear -- `let`, `new` and `switch` read what
//! they name and leave it in the environment, and a `substitute` need not list
//! the names it drops (see its module docs) -- because the bytecode backend
//! collects instead. This pass makes it linear: it works out, at every
//! statement, which names are used again, and
//!
//! - **shares** a name where something consumes it and it is used again, or
//!   is consumed more than once;
//! - **erases** a name the moment nothing uses it again -- on entry to every
//!   statement, before it runs, so memory comes back as early as it can.
//!
//! What it produces, [`L`], consumes exactly as the paper's statements do:
//! `let` its fields, `new` its captures, `switch` its scrutinee (an arm loads
//! the fields), `invoke` the object and the arguments, `jump` the whole
//! environment. A primitive borrows its arguments.
//!
//! A name whose representation is not a reference is tracked the same way --
//! the environment is one list -- but sharing and erasing it does nothing, so
//! no statement is written for it. A name of a type variable's type is shared
//! and erased with the descriptor it is described by, and the emitted code
//! asks the descriptor.

use meadow_seq::{Block, Extern, Label, Name, Program, Rep, Statement, Tag};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A linear statement. See the module docs for what each consumes.
#[derive(Debug, Clone)]
pub enum L {
    /// One reference more to `name`, which stays owned.
    Share(Name, Box<L>),
    /// One reference fewer: `name` is no longer owned.
    Erase(Name, Box<L>),
    /// Each `(to, from)`: `to` is now what `from` was, and `from` is gone. What
    /// a `substitute` leaves once its shares and erases are written: a
    /// renaming, which emits nothing.
    Rename(Vec<(Name, Name)>, Box<L>),
    Let {
        name: Name,
        tag: Tag,
        fields: Vec<Name>,
        rest: Box<L>,
    },
    /// Consumes the scrutinee: each arm binds its fields, which it loads.
    Switch {
        scrutinee: Name,
        arms: Vec<(Tag, Vec<Name>, L)>,
        default: Box<L>,
    },
    New {
        name: Name,
        captures: Vec<Name>,
        methods: Vec<LBlock>,
        /// A continuation made for a non-tail call: see `meadow_seq`'s
        /// `Program::frames`, and `docs/AOT.md` for what becomes of one.
        frame: bool,
        rest: Box<L>,
    },
    Invoke {
        target: Name,
        tag: Tag,
        args: Vec<Name>,
    },
    Jump {
        label: Label,
        args: Vec<Name>,
    },
    /// A primitive: borrows `args`, and runs the continuation it chooses,
    /// which binds its results.
    Extern {
        op: Extern,
        args: Vec<Name>,
        blocks: Vec<(Vec<Name>, L)>,
    },
    Error(&'static str),
}

/// A method: what it binds, and its body, which owns all of it.
#[derive(Debug, Clone)]
pub struct LBlock {
    pub params: Vec<Name>,
    pub body: L,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub msg: String,
}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error { msg: msg.into() })
}

/// A definition's block, linear.
pub fn block(program: &Program, b: &Block) -> Result<LBlock, Error> {
    let mut pass = Pass {
        program,
        uses: HashMap::new(),
    };
    let owned: HashSet<Name> = b.params.iter().copied().collect();
    let body = pass.stmt(&b.body, &b.params, owned)?;
    Ok(LBlock {
        params: b.params.clone(),
        body,
    })
}

struct Pass<'p> {
    program: &'p Program,
    /// What each statement uses, by its address: the environment a statement
    /// runs in is fixed by where it is, so this is a function of the node.
    uses: HashMap<*const Statement, Rc<HashSet<Name>>>,
}

impl<'p> Pass<'p> {
    /// Is sharing or erasing `n` anything at all?
    fn counted(&self, n: Name) -> bool {
        matches!(
            self.program.reps.get(&n),
            Some(Rep::Ref | Rep::Var(_)) | None
        )
    }

    /// The names of the environment `env` that `s`, run in it, uses.
    ///
    /// `jump` and `invoke` hand the environment on whole, so they use all of
    /// it. A block entered with values is asked what it uses of its
    /// parameters, and those are mapped back to the values it was entered
    /// with -- which is what makes a `substitute`'s unused selections dead
    /// before it, and not only after.
    fn uses(&mut self, s: &'p Statement, env: &[Name]) -> Rc<HashSet<Name>> {
        let key = s as *const Statement;
        if let Some(u) = self.uses.get(&key) {
            return u.clone();
        }
        let mut out = HashSet::new();
        match s {
            Statement::Substitute(sel, block) => {
                let inner = self.uses(&block.body, &block.params);
                for (p, v) in block.params.iter().zip(sel) {
                    if inner.contains(p) {
                        out.insert(*v);
                    }
                }
            }
            Statement::Jump(_) | Statement::Invoke(..) => out.extend(env.iter().copied()),
            Statement::Let {
                name, fields, rest, ..
            } => {
                out.extend(fields.iter().copied());
                let inner = self.uses(rest, &cons(*name, env));
                out.extend(inner.iter().filter(|n| *n != name).copied());
            }
            Statement::New {
                name,
                captures,
                rest,
                ..
            } => {
                out.extend(captures.iter().copied());
                let inner = self.uses(rest, &cons(*name, env));
                out.extend(inner.iter().filter(|n| *n != name).copied());
            }
            Statement::Switch {
                scrutinee,
                arms,
                default,
            } => {
                out.insert(*scrutinee);
                let d = self.entered(default, &[], env);
                out.extend(d);
                let fields_of = |b: &Block| b.params.len().saturating_sub(default.params.len());
                for (_, arm) in arms {
                    let n = fields_of(arm);
                    let u = self.entered(arm, &vec![None; n], env);
                    out.extend(u);
                }
            }
            Statement::Extern { args, blocks, .. } => {
                out.extend(args.iter().copied());
                for b in blocks {
                    let n = b.params.len().saturating_sub(env.len());
                    let u = self.entered(b, &vec![None; n], env);
                    out.extend(u);
                }
            }
            Statement::Mark(_, inner) => {
                let u = self.uses(inner, env);
                out.extend(u.iter().copied());
            }
            Statement::Error(_) => {}
        }
        let out = Rc::new(out);
        self.uses.insert(key, out.clone());
        out
    }

    /// What of `env` a block uses, entered with `fresh` values (which are not
    /// `env`'s: fields, results) followed by `env`.
    fn entered(&mut self, b: &'p Block, fresh: &[Option<Name>], env: &[Name]) -> Vec<Name> {
        let inner = self.uses(&b.body, &b.params);
        let vals: Vec<Option<Name>> = fresh
            .iter()
            .copied()
            .chain(env.iter().copied().map(Some))
            .collect();
        b.params
            .iter()
            .zip(vals)
            .filter_map(|(p, v)| if inner.contains(p) { v } else { None })
            .collect()
    }

    /// `s`, run in `env`, of which `owned` is what is owned -- each once.
    fn stmt(
        &mut self,
        s: &'p Statement,
        env: &[Name],
        mut owned: HashSet<Name>,
    ) -> Result<L, Error> {
        // Erase what nothing uses again, before anything runs.
        let used = self.uses(s, env);
        let mut dead: Vec<Name> = env
            .iter()
            .copied()
            .filter(|n| owned.contains(n) && !used.contains(n))
            .collect();
        dead.dedup();
        for n in &dead {
            owned.remove(n);
        }
        let body = self.live(s, env, owned)?;
        Ok(self.erasing(&dead, body))
    }

    /// Wrap `body` in erases of `names`, those that count.
    fn erasing(&self, names: &[Name], mut body: L) -> L {
        for n in names.iter().rev() {
            if self.counted(*n) {
                body = L::Erase(*n, Box::new(body));
            }
        }
        body
    }

    fn sharing(&self, n: Name, times: usize, mut body: L) -> L {
        if self.counted(n) {
            for _ in 0..times {
                body = L::Share(n, Box::new(body));
            }
        }
        body
    }

    /// Consume `names` -- each occurrence once -- where `after` is what is
    /// used once they have been: share what is consumed more than once or
    /// used again, and give up the rest. Answers how many shares of each.
    fn consume(
        &self,
        names: &[Name],
        after: &HashSet<Name>,
        owned: &mut HashSet<Name>,
    ) -> Result<Vec<(Name, usize)>, Error> {
        let mut counts: Vec<(Name, usize)> = Vec::new();
        for n in names {
            match counts.iter_mut().find(|(m, _)| m == n) {
                Some((_, c)) => *c += 1,
                None => counts.push((*n, 1)),
            }
        }
        let mut shares = Vec::new();
        for (n, c) in counts {
            if !owned.contains(&n) {
                return err(format!("{n:?} is consumed but not owned"));
            }
            let again = after.contains(&n);
            if !again {
                owned.remove(&n);
            }
            shares.push((n, c - 1 + usize::from(again)));
        }
        Ok(shares)
    }

    fn wrap_shares(&self, shares: &[(Name, usize)], mut body: L) -> L {
        for (n, times) in shares.iter().rev() {
            body = self.sharing(*n, *times, body);
        }
        body
    }

    /// `s`, whose environment holds only what it uses.
    fn live(&mut self, s: &'p Statement, env: &[Name], owned: HashSet<Name>) -> Result<L, Error> {
        let mut owned = owned;
        match s {
            Statement::Mark(_, inner) => self.live(inner, env, owned),
            Statement::Error(msg) => Ok(L::Error(msg)),

            Statement::Substitute(sel, block) => {
                let inner = self.uses(&block.body, &block.params);
                // A selection whose parameter the block does not use is as good
                // as left out; `stmt` has erased those already, since `uses`
                // said so, unless the same value is selected again for a
                // parameter that is used.
                let taken: Vec<Name> = block
                    .params
                    .iter()
                    .zip(sel)
                    .filter(|(p, _)| inner.contains(p))
                    .map(|(_, v)| *v)
                    .collect();
                let shares = self.consume(&taken, &HashSet::new(), &mut owned)?;
                let binds: Vec<(Name, Name)> = block
                    .params
                    .iter()
                    .zip(sel)
                    .filter(|(p, _)| inner.contains(p))
                    .map(|(p, v)| (*p, *v))
                    .collect();
                let owned_in: HashSet<Name> = binds.iter().map(|(p, _)| *p).collect();
                let body = self.stmt(&block.body, &block.params, owned_in)?;
                let body = L::Rename(binds, Box::new(body));
                Ok(self.wrap_shares(&shares, body))
            }

            Statement::Jump(label) => {
                for n in env {
                    if !owned.contains(n) {
                        return err(format!("a jump hands on {n:?}, which is not owned"));
                    }
                }
                Ok(L::Jump {
                    label: *label,
                    args: env.to_vec(),
                })
            }

            Statement::Invoke(target, tag) => {
                for n in env {
                    if !owned.contains(n) {
                        return err(format!("an invoke hands on {n:?}, which is not owned"));
                    }
                }
                let mut args = env.to_vec();
                if let Some(i) = args.iter().position(|n| n == target) {
                    args.remove(i);
                }
                Ok(L::Invoke {
                    target: *target,
                    tag: *tag,
                    args,
                })
            }

            Statement::Let {
                name,
                tag,
                fields,
                rest,
                ..
            } => {
                let env2 = cons(*name, env);
                let after = self.uses(rest, &env2);
                let shares = self.consume(fields, &after, &mut owned)?;
                owned.insert(*name);
                let body = self.stmt(rest, &env2, owned)?;
                Ok(self.wrap_shares(
                    &shares,
                    L::Let {
                        name: *name,
                        tag: *tag,
                        fields: fields.clone(),
                        rest: Box::new(body),
                    },
                ))
            }

            Statement::New {
                name,
                captures,
                methods,
                rest,
            } => {
                let env2 = cons(*name, env);
                let after = self.uses(rest, &env2);
                let shares = self.consume(captures, &after, &mut owned)?;
                let mut ms = Vec::with_capacity(methods.len());
                for m in methods {
                    ms.push(block(self.program, m)?);
                }
                owned.insert(*name);
                let body = self.stmt(rest, &env2, owned)?;
                Ok(self.wrap_shares(
                    &shares,
                    L::New {
                        name: *name,
                        captures: captures.clone(),
                        methods: ms,
                        frame: self.program.frames.contains(name),
                        rest: Box::new(body),
                    },
                ))
            }

            Statement::Switch {
                scrutinee,
                arms,
                default,
            } => {
                // The switch consumes the scrutinee; an arm that still wants it
                // -- `match o with | Just x -> o` -- gets a share.
                let wanted = arms
                    .iter()
                    .map(|(_, b)| b)
                    .chain(std::iter::once(&**default))
                    .any(|b| self.uses(&b.body, &b.params).contains(scrutinee));
                if !owned.contains(scrutinee) {
                    return err(format!("{scrutinee:?} is switched on but not owned"));
                }
                if !wanted {
                    owned.remove(scrutinee);
                }
                let nfields = |b: &Block| b.params.len().saturating_sub(default.params.len());
                let mut larms = Vec::with_capacity(arms.len());
                for (tag, arm) in arms {
                    let n = nfields(arm);
                    let fields = arm.params[..n].to_vec();
                    let body = self.enter(arm, n, env, &owned)?;
                    larms.push((*tag, fields, body));
                }
                let ldefault = self.enter(default, 0, env, &owned)?;
                let sw = L::Switch {
                    scrutinee: *scrutinee,
                    arms: larms,
                    default: Box::new(ldefault),
                };
                Ok(if wanted {
                    self.sharing(*scrutinee, 1, sw)
                } else {
                    sw
                })
            }

            Statement::Extern { op, args, blocks } => {
                for a in args {
                    if !owned.contains(a) {
                        return err(format!("a primitive reads {a:?}, which is not owned"));
                    }
                }
                let mut lblocks = Vec::with_capacity(blocks.len());
                for b in blocks {
                    let n = b.params.len().saturating_sub(env.len());
                    let results = b.params[..n].to_vec();
                    let body = self.enter(b, n, env, &owned)?;
                    lblocks.push((results, body));
                }
                Ok(L::Extern {
                    op: op.clone(),
                    args: args.clone(),
                    blocks: lblocks,
                })
            }
        }
    }

    /// A continuation within the same activation: its first `fresh`
    /// parameters are new values, owned, and the rest are `env` again -- by
    /// the same names, or renamed.
    fn enter(
        &mut self,
        b: &'p Block,
        fresh: usize,
        env: &[Name],
        owned: &HashSet<Name>,
    ) -> Result<L, Error> {
        let mut owned_in: HashSet<Name> = b.params[..fresh].iter().copied().collect();
        let mut renames = Vec::new();
        for (p, v) in b.params[fresh..].iter().zip(env) {
            if owned.contains(v) {
                owned_in.insert(*p);
                if p != v {
                    renames.push((*p, *v));
                }
            }
        }
        let body = self.stmt(&b.body, &b.params, owned_in)?;
        Ok(if renames.is_empty() {
            body
        } else {
            L::Rename(renames, Box::new(body))
        })
    }
}

fn cons(n: Name, env: &[Name]) -> Vec<Name> {
    let mut v = Vec::with_capacity(env.len() + 1);
    v.push(n);
    v.extend_from_slice(env);
    v
}

/// The names `l` uses that it does not bind, in the order it first uses
/// them: what running it needs from where it is -- and so what packing it as
/// a closure captures.
pub fn free(l: &L) -> Vec<Name> {
    fn go(l: &L, bound: &mut Vec<Name>, out: &mut Vec<Name>) {
        let use_ = |n: Name, bound: &Vec<Name>, out: &mut Vec<Name>| {
            if !bound.contains(&n) && !out.contains(&n) {
                out.push(n);
            }
        };
        match l {
            L::Share(n, rest) | L::Erase(n, rest) => {
                use_(*n, bound, out);
                go(rest, bound, out);
            }
            L::Rename(binds, rest) => {
                for (_, from) in binds {
                    use_(*from, bound, out);
                }
                let mark = bound.len();
                bound.extend(binds.iter().map(|(to, _)| *to));
                go(rest, bound, out);
                bound.truncate(mark);
            }
            L::Let {
                name, fields, rest, ..
            } => {
                for f in fields {
                    use_(*f, bound, out);
                }
                bound.push(*name);
                go(rest, bound, out);
                bound.pop();
            }
            L::New {
                name,
                captures,
                rest,
                ..
            } => {
                for c in captures {
                    use_(*c, bound, out);
                }
                bound.push(*name);
                go(rest, bound, out);
                bound.pop();
            }
            L::Switch {
                scrutinee,
                arms,
                default,
            } => {
                use_(*scrutinee, bound, out);
                for (_, fields, body) in arms {
                    let mark = bound.len();
                    bound.extend(fields.iter().copied());
                    go(body, bound, out);
                    bound.truncate(mark);
                }
                go(default, bound, out);
            }
            L::Invoke { target, args, .. } => {
                use_(*target, bound, out);
                for a in args {
                    use_(*a, bound, out);
                }
            }
            L::Jump { args, .. } => {
                for a in args {
                    use_(*a, bound, out);
                }
            }
            L::Extern { args, blocks, .. } => {
                for a in args {
                    use_(*a, bound, out);
                }
                for (results, body) in blocks {
                    let mark = bound.len();
                    bound.extend(results.iter().copied());
                    go(body, bound, out);
                    bound.truncate(mark);
                }
            }
            L::Error(_) => {}
        }
    }
    let mut out = Vec::new();
    go(l, &mut Vec::new(), &mut out);
    out
}

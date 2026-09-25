//! **Linearization**: AxCut with every share and every erase written down.
//!
//! In the AxCut paper (see `docs/SILO.md`) the environment is linear: every
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
//! A `switch` decides per arm. An arm that still wants the scrutinee --
//! `match o with | Just x -> o`, or an arm of a decision tree that tests a
//! value and then uses it whole -- *keeps* it: it borrows the fields it uses,
//! sharing each, and the scrutinee stays its own. Every other arm consumes
//! the scrutinee, and so is the one that can build in its block. Deciding for
//! the whole `switch` at once, as this pass used to, shared the scrutinee
//! before it whenever any arm wanted it, and no arm could reuse it.
//!
//! A kept scrutinee can still be reused on a path that no longer wants it:
//! where it is erased there, and something after builds a block its size,
//! the erase is a `DropReuse` (Perceus's *drop-reuse*) -- when it was the
//! last reference, its references to its fields are given up and its block
//! is the token; when it was not, it is erased as usual and the token holds
//! nothing. `if k < x then … (ins l k) … else t` keeps `t` for the one path
//! that returns it, and rebuilds in it on the others.
//!
//! **Reuse** (FBIP, after Perceus). An arm that takes a block apart and then,
//! before anything else could want the memory, builds a block of the same size
//! is given the old one to build in: `switch` binds a *reuse token* in the arm
//! -- the scrutinee's block when it was the last reference, whose fields the
//! arm has just been handed, or nothing when it was shared -- and the first
//! `let` of as many fields on each path out of the arm builds in it. A path
//! that builds nothing that size gives the token back (`Clean`) where it
//! parts from the paths that do, so the memory is never held longer than it
//! could be used. `map` over a list it holds the only reference to rebuilds
//! the list in place.
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
        /// A reuse token to build in, when it holds a block: see the module
        /// docs.
        reuse: Option<Name>,
        rest: Box<L>,
    },
    /// Each arm binds its fields, which it loads, and either consumes the
    /// scrutinee -- binding, when something in it can build in the
    /// scrutinee's block, a reuse token -- or keeps it (see the module docs).
    /// The default consumes it too, unless `keep_default`.
    Switch {
        scrutinee: Name,
        arms: Vec<SwitchArm>,
        default: Box<L>,
        keep_default: bool,
    },
    /// Give the reuse token `name` back unused: its block, if it holds one,
    /// goes back to the runtime, its fields already taken.
    Clean(Name, Box<L>),
    /// Erase `name`, a block whose fields a kept arm loaded as `fields`, and
    /// bind `token`: `name`'s block, its references to `fields` given up,
    /// when this was the last reference to it, and nothing when it was not.
    DropReuse {
        name: Name,
        fields: Vec<Name>,
        token: Name,
        rest: Box<L>,
    },
    New {
        name: Name,
        captures: Vec<Name>,
        methods: Vec<LBlock>,
        /// A continuation made for a non-tail call: see `meadow_seq`'s
        /// `Program::frames`, and `docs/SILO.md` for what becomes of one.
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

/// An arm of a [`L::Switch`].
#[derive(Debug, Clone)]
pub struct SwitchArm {
    pub tag: Tag,
    pub fields: Vec<Name>,
    /// The token for the scrutinee's block, in an arm that consumes it.
    pub reuse: Option<Name>,
    /// Whether the arm keeps the scrutinee rather than consuming it: its
    /// body shares what it uses of the fields, which are borrowed.
    pub keep: bool,
    pub body: L,
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
    let mut tokens = 0;
    block_in(program, b, &mut tokens)
}

/// [`block`], numbering reuse tokens on from `tokens`: a method's are counted
/// with its definition's, since a token can be carried into a frame's method
/// and must not meet one of the method's own there.
fn block_in(program: &Program, b: &Block, tokens: &mut u32) -> Result<LBlock, Error> {
    let mut pass = Pass {
        program,
        uses: HashMap::new(),
        tokens: *tokens,
    };
    let owned: HashSet<Name> = b.params.iter().copied().collect();
    let body = pass.stmt(&b.body, &b.params, owned)?;
    *tokens = pass.tokens;
    Ok(LBlock {
        params: b.params.clone(),
        body,
    })
}

/// Whether `n` is a reuse token: they are numbered down from the top of the
/// id space, where no program's names are.
pub fn is_token(n: Name) -> bool {
    n.0 > u32::MAX - (1 << 24)
}

struct Pass<'p> {
    program: &'p Program,
    /// What each statement uses, by its address: the environment a statement
    /// runs in is fixed by where it is, so this is a function of the node.
    uses: HashMap<*const Statement, Rc<HashSet<Name>>>,
    /// How many reuse tokens this definition has made: each is a name of its
    /// own, from the top of the id space down, where no program's are.
    tokens: u32,
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
                        reuse: None,
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
                    ms.push(block_in(self.program, m, &mut self.tokens)?);
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
                if !owned.contains(scrutinee) {
                    return err(format!("{scrutinee:?} is switched on but not owned"));
                }
                let mut consumed = owned.clone();
                consumed.remove(scrutinee);
                let nfields = |b: &Block| b.params.len().saturating_sub(default.params.len());
                let mut larms = Vec::with_capacity(arms.len());
                for (tag, arm) in arms {
                    let n = nfields(arm);
                    let fields = arm.params[..n].to_vec();
                    let keep = self.entered(arm, &vec![None; n], env).contains(scrutinee);
                    let (reuse, body) = if keep {
                        // Borrowed fields: what the arm uses of them is
                        // shared, and the rest are never its own.
                        let used = self.uses(&arm.body, &arm.params);
                        let taken: Vec<Name> = fields
                            .iter()
                            .copied()
                            .filter(|x| used.contains(x))
                            .collect();
                        let body = self.enter_taking(arm, n, &taken, env, &owned)?;
                        // Where the scrutinee dies on a path that builds a
                        // block its size, it is dropped into a token.
                        let body = if n > 0 && reusing() {
                            let mut names = vec![*scrutinee];
                            names.extend(
                                arm.params[n..]
                                    .iter()
                                    .zip(env)
                                    .filter(|(_, v)| *v == scrutinee)
                                    .map(|(p, _)| *p),
                            );
                            self.drop_reuse(body, &names, &fields)
                        } else {
                            body
                        };
                        let body = taken.iter().rev().fold(body, |b, x| self.sharing(*x, 1, b));
                        (None, body)
                    } else {
                        let body = self.enter(arm, n, env, &consumed)?;
                        if n > 0 && reusing() && has_site(&body, n) {
                            let t = self.token();
                            (Some(t), self.place(body, t, n))
                        } else {
                            (None, body)
                        }
                    };
                    larms.push(SwitchArm {
                        tag: *tag,
                        fields,
                        reuse,
                        keep,
                        body,
                    });
                }
                let keep_default = self.entered(default, &[], env).contains(scrutinee);
                let ldefault = self.enter(
                    default,
                    0,
                    env,
                    if keep_default { &owned } else { &consumed },
                )?;
                Ok(L::Switch {
                    scrutinee: *scrutinee,
                    arms: larms,
                    default: Box::new(ldefault),
                    keep_default,
                })
            }

            Statement::Extern { op, args, blocks } => {
                for a in args {
                    if !owned.contains(a) {
                        return err(format!("a primitive reads {a:?}, which is not owned"));
                    }
                }
                // A primitive that consumes an argument is handed a reference
                // of its own: a share of it where anything after still wants
                // it, and otherwise the caller's, which is then no longer the
                // caller's to erase. See `emit::consumes`.
                let mut owned = owned;
                let mut share_first = None;
                if let Some(i) = crate::emit::consumes(op)
                    && let Some(&a) = args.get(i)
                    && self.counted(a)
                {
                    let again = args.iter().filter(|x| **x == a).count() > 1
                        || blocks.iter().any(|b| {
                            let n = b.params.len().saturating_sub(env.len());
                            let used = self.uses(&b.body, &b.params);
                            b.params[n..]
                                .iter()
                                .zip(env)
                                .any(|(p, v)| *v == a && used.contains(p))
                        });
                    if again {
                        share_first = Some(a);
                    } else {
                        owned.remove(&a);
                    }
                }
                let mut lblocks = Vec::with_capacity(blocks.len());
                for b in blocks {
                    let n = b.params.len().saturating_sub(env.len());
                    let results = b.params[..n].to_vec();
                    let body = self.enter(b, n, env, &owned)?;
                    lblocks.push((results, body));
                }
                let ext = L::Extern {
                    op: op.clone(),
                    args: args.clone(),
                    blocks: lblocks,
                };
                Ok(match share_first {
                    Some(a) => self.sharing(a, 1, ext),
                    None => ext,
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
        self.enter_taking(b, fresh, &b.params[..fresh], env, owned)
    }

    /// [`Self::enter`], owning only `taken` of the new values.
    fn enter_taking(
        &mut self,
        b: &'p Block,
        fresh: usize,
        taken: &[Name],
        env: &[Name],
        owned: &HashSet<Name>,
    ) -> Result<L, Error> {
        let mut owned_in: HashSet<Name> = taken.iter().copied().collect();
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
                name,
                fields,
                reuse,
                rest,
                ..
            } => {
                for f in fields {
                    use_(*f, bound, out);
                }
                if let Some(t) = reuse {
                    use_(*t, bound, out);
                }
                bound.push(*name);
                go(rest, bound, out);
                bound.pop();
            }
            L::Clean(t, rest) => {
                use_(*t, bound, out);
                go(rest, bound, out);
            }
            L::DropReuse {
                name,
                fields,
                token,
                rest,
            } => {
                use_(*name, bound, out);
                for f in fields {
                    use_(*f, bound, out);
                }
                bound.push(*token);
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
                ..
            } => {
                use_(*scrutinee, bound, out);
                for a in arms {
                    let mark = bound.len();
                    bound.extend(a.fields.iter().copied());
                    bound.extend(a.reuse.iter().copied());
                    go(&a.body, bound, out);
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

// --- reuse ---------------------------------------------------------------------------

/// Whether to reuse at all: always, unless `MEADOW_NO_REUSE` is set -- which is
/// for measuring what reuse is worth, the same compiler with it and without.
fn reusing() -> bool {
    std::env::var_os("MEADOW_NO_REUSE").is_none()
}

/// Whether a primitive runs the code after it later, as a closure: nothing may
/// be carried into that code but what a closure captures, and a reuse token is
/// not a value.
fn packs(op: &Extern) -> bool {
    matches!(
        op,
        Extern::Prim(
            meadow_core::Prim::Enter | meadow_core::Prim::Detach | meadow_core::Prim::Reattach
        )
    )
}

/// The method of `l` when it is a frame a token can be carried into: the
/// continuation of a non-tail call, entered once with the call's one answer.
/// Its code runs after the call, in the same activation as far as the token
/// is concerned -- which is what lets `Node c (ins l k v) kx vx r` build in
/// the node it took apart before the call.
fn frame_method(l: &L) -> Option<&LBlock> {
    match l {
        L::New {
            captures,
            methods,
            frame: true,
            ..
        } if methods.len() == 1 && methods[0].params.len() == captures.len() + 1 => {
            Some(&methods[0])
        }
        _ => None,
    }
}

/// Whether some path through `l` builds a block of `n` fields that has no
/// block to build in yet -- in `l` itself or in the frames it makes, which run
/// once; not in the code of a closure or a packed continuation, which may run
/// any number of times, or none.
fn has_site(l: &L, n: usize) -> bool {
    match l {
        L::Let {
            fields,
            reuse,
            rest,
            ..
        } => (fields.len() == n && reuse.is_none()) || has_site(rest, n),
        L::Share(_, rest) | L::Erase(_, rest) | L::Rename(_, rest) | L::Clean(_, rest) => {
            has_site(rest, n)
        }
        L::DropReuse { rest, .. } => has_site(rest, n),
        L::New { rest, .. } => {
            has_site(rest, n) || frame_method(l).is_some_and(|m| has_site(&m.body, n))
        }
        L::Switch { arms, default, .. } => {
            arms.iter().any(|a| has_site(&a.body, n)) || has_site(default, n)
        }
        L::Extern { op, blocks, .. } => !packs(op) && blocks.iter().any(|(_, b)| has_site(b, n)),
        L::Invoke { .. } | L::Jump { .. } | L::Error(_) => false,
    }
}

impl Pass<'_> {
    /// `l`, the body of an arm that keeps a scrutinee known inside it as
    /// `names`, whose fields it loaded as `fields`: each erase of the
    /// scrutinee followed by a `let` of as many fields made a `DropReuse`,
    /// whose token that `let` builds in. See the module docs.
    fn drop_reuse(&mut self, l: L, names: &[Name], fields: &[Name]) -> L {
        let n = fields.len();
        let on = |this: &mut Self, rest: Box<L>| Box::new(this.drop_reuse(*rest, names, fields));
        match l {
            L::Erase(x, rest) if names.contains(&x) && has_site(&rest, n) => {
                let token = self.token();
                let rest = self.place(*rest, token, n);
                L::DropReuse {
                    name: x,
                    fields: fields.to_vec(),
                    token,
                    rest: Box::new(rest),
                }
            }
            L::Share(x, rest) => L::Share(x, on(self, rest)),
            L::Erase(x, rest) => L::Erase(x, on(self, rest)),
            L::Clean(x, rest) => L::Clean(x, on(self, rest)),
            L::Rename(binds, rest) => {
                // The scrutinee under another name, past a renaming.
                let mut more = names.to_vec();
                more.extend(
                    binds
                        .iter()
                        .filter(|(_, from)| names.contains(from))
                        .map(|(to, _)| *to),
                );
                L::Rename(binds, Box::new(self.drop_reuse(*rest, &more, fields)))
            }
            L::Let {
                name,
                tag,
                fields: fs,
                reuse,
                rest,
            } => L::Let {
                name,
                tag,
                fields: fs,
                reuse,
                rest: on(self, rest),
            },
            L::DropReuse {
                name,
                fields: fs,
                token,
                rest,
            } => L::DropReuse {
                name,
                fields: fs,
                token,
                rest: on(self, rest),
            },
            L::New {
                name,
                captures,
                methods,
                frame,
                rest,
            } => L::New {
                name,
                captures,
                methods,
                frame,
                rest: on(self, rest),
            },
            L::Switch {
                scrutinee,
                arms,
                default,
                keep_default,
            } => L::Switch {
                scrutinee,
                arms: arms
                    .into_iter()
                    .map(|a| SwitchArm {
                        body: *on(self, Box::new(a.body)),
                        ..a
                    })
                    .collect(),
                default: on(self, default),
                keep_default,
            },
            L::Extern { op, args, blocks } if !packs(&op) => L::Extern {
                op,
                args,
                blocks: blocks
                    .into_iter()
                    .map(|(rs, body)| (rs, *on(self, Box::new(body))))
                    .collect(),
            },
            other => other,
        }
    }

    /// A new reuse token's name.
    fn token(&mut self) -> Name {
        self.tokens += 1;
        meadow_hir::VarId(u32::MAX - self.tokens)
    }

    /// `l`, with the first `let` of `n` fields on each path building in token
    /// `t`, and every path that builds none giving `t` back where it parts from
    /// those that do. `l` has a site: see [`has_site`].
    fn place(&mut self, l: L, t: Name, n: usize) -> L {
        // `rest` keeps the token going if it can use it, and gives it back
        // first if it cannot.
        let onward = |this: &mut Self, rest: Box<L>| -> Box<L> {
            if has_site(&rest, n) {
                Box::new(this.place(*rest, t, n))
            } else {
                Box::new(L::Clean(t, rest))
            }
        };
        let frame_site = frame_method(&l).is_some_and(|m| has_site(&m.body, n));
        match l {
            L::Let {
                name,
                tag,
                fields,
                reuse: None,
                rest,
            } if fields.len() == n => L::Let {
                name,
                tag,
                fields,
                reuse: Some(t),
                rest,
            },
            L::Let {
                name,
                tag,
                fields,
                reuse,
                rest,
            } => L::Let {
                name,
                tag,
                fields,
                reuse,
                rest: onward(self, rest),
            },
            L::Share(x, rest) => L::Share(x, onward(self, rest)),
            L::Erase(x, rest) => L::Erase(x, onward(self, rest)),
            L::Rename(b, rest) => L::Rename(b, onward(self, rest)),
            L::Clean(x, rest) => L::Clean(x, onward(self, rest)),
            L::DropReuse {
                name,
                fields,
                token,
                rest,
            } => L::DropReuse {
                name,
                fields,
                token,
                rest: onward(self, rest),
            },
            // What runs before the call builds in it if it can; otherwise the
            // frame carries it, as one more capture, to the code after. A
            // path in `rest` that never enters the frame erases it, and the
            // token with it: see `emit`.
            L::New {
                name,
                mut captures,
                mut methods,
                frame,
                rest,
            } if frame_site && !has_site(&rest, n) => {
                let m = methods.pop().expect("a frame has one method");
                let inner = self.token();
                let ncap = captures.len();
                captures.push(t);
                let mut params = m.params;
                params.insert(ncap, inner);
                let body = self.place(m.body, inner, n);
                methods.push(LBlock { params, body });
                L::New {
                    name,
                    captures,
                    methods,
                    frame,
                    rest,
                }
            }
            L::New {
                name,
                captures,
                methods,
                frame,
                rest,
            } => L::New {
                name,
                captures,
                methods,
                frame,
                rest: onward(self, rest),
            },
            L::Switch {
                scrutinee,
                arms,
                default,
                keep_default,
            } => L::Switch {
                scrutinee,
                arms: arms
                    .into_iter()
                    .map(|a| SwitchArm {
                        body: *onward(self, Box::new(a.body)),
                        ..a
                    })
                    .collect(),
                default: onward(self, default),
                keep_default,
            },
            L::Extern { op, args, blocks } if !packs(&op) => L::Extern {
                op,
                args,
                blocks: blocks
                    .into_iter()
                    .map(|(rs, body)| (rs, *onward(self, Box::new(body))))
                    .collect(),
            },
            other => L::Clean(t, Box::new(other)),
        }
    }
}

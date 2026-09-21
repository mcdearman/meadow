//! **Traits**: what a `where` asks for, and how it is answered.
//!
//! A trait is compiled to a *dictionary* -- a value holding its methods for one
//! type -- and a `where Show a` to a parameter that takes one. So inference has
//! two jobs here beyond types. It has to work out which dictionary every
//! mention of a constrained name is applied to, which is the [`Evidence`] it
//! hands lowering; and it has to work out which traits a function nobody wrote
//! a signature for needs of its type variables, which become the `where` of
//! its scheme.
//!
//! # Wanted, and given
//!
//! Instantiating a scheme with a `where` leaves a **wanted** [`Pred`] for each
//! trait it names, over the fresh variables the instantiation made. Nothing is
//! done about one until its type is known, because which `impl` answers it is a
//! question about the type's outermost constructor:
//!
//! * a type with a constructor is answered by the one `impl` for that trait and
//!   constructor, whose own `where` becomes more wanteds;
//! * a type that is still a variable when its function is generalized is the
//!   function's to ask its caller for: it joins the scheme's `where`, and the
//!   wanted is answered by that parameter. With a signature, the `where` is
//!   what the signature **gives**, and a wanted it does not cover is an error;
//! * a type that is still a variable and is *not* the function's to generalize
//!   waits -- for the binding around it, or for a number to be defaulted -- and
//!   is an ambiguity if it is still waiting when the unit is done.
//!
//! Only a top-level `fun` takes dictionaries. A `def` is made once, which a
//! function of a dictionary is not, and a local function's needs are left to
//! the function around it: both simply do not generalize a variable something
//! is still wanted of.
//!
//! # Associated types
//!
//! `Elem f` is never a type application to reduce. A [`Pred`] carries its
//! trait's associated types beside the type they are of, as types of their
//! own: a scheme over `Container f` is over one more variable for `Elem f`,
//! and unifying a wanted with the `impl` that answers it is what settles that
//! variable. Two predicates of one trait at one type have the same associated
//! types, which is the other way they are settled. So where a program writes
//! `Elem f`, [`Infer::lift_assocs`] puts the variable, and nothing downstream
//! ever meets an associated type.

use super::*;

/// How a wanted trait is answered, as lowering builds it.
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    /// The enclosing top-level function's own dictionary parameter, by its
    /// place in that function's `where`.
    Given(usize),
    /// A required trait's dictionary, out of the dictionary of a trait that
    /// requires it: field `label`, whose type is `ty`.
    Super {
        of: Box<Evidence>,
        label: InternedString,
        ty: Type,
    },
    /// An `impl`'s dictionary -- `dict`, at the dictionary type `ty` -- applied
    /// to the dictionaries its own `where` asks for.
    Impl {
        dict: VarId,
        ty: Type,
        args: Vec<Evidence>,
    },
    /// Nothing answered it; reported already.
    Missing,
}

/// A trait as lowering needs it: the layout of its dictionary.
#[derive(Debug, Clone, Default)]
pub struct TraitShape {
    /// How many types it is a trait of.
    pub params: usize,
    /// The traits it requires, each of which of its parameters.
    pub supers: Vec<(InternedString, Vec<usize>)>,
    pub assocs: Vec<InternedString>,
    /// `(name, the value a program calls, its default's definition)`.
    pub methods: Vec<(InternedString, VarId, Option<VarId>)>,
    /// The dictionary's constructor, and its fields' labels: one per required
    /// trait, then one per method.
    pub dict: InternedString,
    pub labels: Vec<InternedString>,
}

pub type TraitEnv = HashMap<InternedString, TraitShape>;

/// The label of the field holding a required trait's dictionary.
pub fn super_label(tr: InternedString) -> InternedString {
    InternedString::from(format!("#super.{tr}"))
}

#[derive(Debug, Clone)]
struct ImplDef {
    dict: VarId,
    /// `forall vars. context => Trait head assocs…`
    scheme: Scheme,
}

#[derive(Debug, Clone)]
enum Sol {
    /// Place in the owning function's `where`, then the required traits to go
    /// through from there, each with the parameters it is asked of.
    ///
    /// With the types that parameter's trait is of, which a required trait's
    /// are selected from.
    Given(usize, Vec<Type>, Vec<(InternedString, Vec<usize>)>),
    Impl {
        dict: VarId,
        ty: Type,
        subs: Vec<usize>,
    },
    Failed,
}

#[derive(Debug, Clone)]
struct Wanted {
    pred: Pred,
    span: Span,
    /// The top-level binding whose body wants it.
    owner: Option<VarId>,
    sol: Option<Sol>,
}

#[derive(Default)]
pub(crate) struct State {
    shapes: TraitEnv,
    /// The scheme of each method, by trait and name.
    methods: HashMap<(InternedString, InternedString), Scheme>,
    /// Associated type -> its trait, and its place among the trait's.
    assoc_of: HashMap<InternedString, (InternedString, usize)>,
    /// By trait and the implementing type's outermost constructor.
    impls: HashMap<(InternedString, String), ImplDef>,
    wanted: Vec<Wanted>,
    at_node: HashMap<NodeId, Vec<usize>>,
    /// Evidence settled outright: a recursive mention's, an `impl`'s.
    fixed: HashMap<NodeId, Vec<Evidence>>,
    /// What the signatures of the bindings being inferred give.
    pub(crate) givens: Vec<Pred>,
    /// The top-level bindings being inferred together, and which one's body
    /// inference is in.
    pub(crate) group: HashSet<VarId>,
    pub(crate) member: Option<VarId>,
    /// Mentions of a member of the group from inside it, which are
    /// monomorphic, and so wanted nothing: `(node, mentioned, from, where)`.
    mentions: Vec<(NodeId, VarId, Option<VarId>, Span)>,
    /// The type a method of an `impl`, or a default, is held to.
    pub(crate) method_sigs: HashMap<VarId, (Scheme, Span)>,
}

impl State {
    pub(crate) fn env(&self) -> TraitEnv {
        self.shapes.clone()
    }
}

/// What an `impl` is found by: the outermost constructor of a type.
fn head_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Con(name, _) => Some(name.to_string()),
        Type::Tuple(items) => Some(format!("#tuple{}", items.len())),
        Type::Fun(..) => Some("#fun".to_string()),
        Type::Record(_) => Some("#record".to_string()),
        _ => None,
    }
}

impl Infer {
    fn trait_error(&mut self, msg: String, label: &str, span: Span) {
        self.errors.push(Diagnostic {
            msg,
            filename: self.filename.clone(),
            label: (label.to_string(), span),
            extra_labels: vec![],
        });
    }

    // --- instantiating --------------------------------------------------------

    /// [`Infer::instantiate`], with the scheme's `where` over the same fresh
    /// variables.
    pub(crate) fn instantiate_with_preds(&mut self, scheme: &Scheme) -> (Type, Vec<Pred>) {
        let fresh: Vec<Type> = scheme
            .quant
            .iter()
            .map(|k| self.arena.fresh_of(*k))
            .collect();
        let preds = scheme
            .preds
            .iter()
            .map(|p| Pred {
                tr: p.tr,
                tys: p
                    .tys
                    .iter()
                    .map(|t| Arena::subst_bound(t, &fresh))
                    .collect(),
                assocs: p
                    .assocs
                    .iter()
                    .map(|a| Arena::subst_bound(a, &fresh))
                    .collect(),
            })
            .collect();
        (Arena::subst_bound(&scheme.ty, &fresh), preds)
    }

    /// Instantiate the scheme of the name mentioned at `node`, wanting what its
    /// `where` asks for.
    pub(crate) fn instantiate_at(&mut self, node: NodeId, span: Span, scheme: &Scheme) -> Type {
        if scheme.preds.is_empty() {
            return self.instantiate(scheme);
        }
        let (ty, preds) = self.instantiate_with_preds(scheme);
        let owner = self.tr.member;
        let mut at = Vec::with_capacity(preds.len());
        for pred in preds {
            at.push(self.tr.wanted.len());
            self.tr.wanted.push(Wanted {
                pred,
                span,
                owner,
                sol: None,
            });
        }
        self.tr.at_node.insert(node, at);
        ty
    }

    /// A mention of `callee`, whose scheme is not known yet because it is being
    /// inferred along with the function the mention is in.
    pub(crate) fn mention_in_group(&mut self, node: NodeId, callee: VarId) {
        if self.tr.group.contains(&callee)
            && self
                .env
                .get(&callee)
                .is_some_and(|s| s.quant.is_empty() && s.preds.is_empty())
        {
            let from = self.tr.member;
            self.tr
                .mentions
                .push((node, callee, from, Span::from(0..0)));
        }
    }

    // --- solving --------------------------------------------------------------

    /// A binding begins: what is wanted from here on is its body's.
    pub(crate) fn begin_binding(&mut self) -> usize {
        self.tr.givens.clear();
        self.tr.mentions.clear();
        self.tr.wanted.len()
    }

    /// Answer every wanted whose type is known by now, and settle associated
    /// types that have to agree. Called inside the level of the binding being
    /// inferred, so that the variables an `impl` brings are that binding's.
    pub(crate) fn solve_wanted(&mut self) {
        loop {
            let mut progress = false;
            for i in 0..self.tr.wanted.len() {
                if self.tr.wanted[i].sol.is_some() {
                    continue;
                }
                let (tr, span) = (self.tr.wanted[i].pred.tr, self.tr.wanted[i].span);
                let tys: Vec<Type> = self.tr.wanted[i]
                    .pred
                    .tys
                    .clone()
                    .iter()
                    .map(|t| self.arena.zonk(t))
                    .collect();
                if tys.iter().any(|t| t.references_error()) {
                    self.tr.wanted[i].sol = Some(Sol::Failed);
                    continue;
                }
                // Every one of them known, outermost constructor at least: an
                // `impl` is found by all of them together.
                let Some(key) = heads_key(&tys) else {
                    self.agree(i, &tys);
                    continue;
                };
                progress = true;
                let Some(found) = self.tr.impls.get(&(tr, key)).cloned() else {
                    self.trait_error(
                        format!(
                            "`{}` does not implement `{}`",
                            show_all(&tys),
                            hir::spelling(&tr)
                        ),
                        &format!("needs `impl {} …` for this type", hir::spelling(&tr)),
                        span,
                    );
                    self.tr.wanted[i].sol = Some(Sol::Failed);
                    continue;
                };
                let (dict_ty, context) = self.instantiate_with_preds(&found.scheme);
                let mut args = tys;
                args.extend(self.tr.wanted[i].pred.assocs.iter().cloned());
                self.unify_at(span, dict_ty.clone(), Type::Con(tr, args));
                let owner = self.tr.wanted[i].owner;
                let mut subs = Vec::with_capacity(context.len());
                for pred in context {
                    subs.push(self.tr.wanted.len());
                    self.tr.wanted.push(Wanted {
                        pred,
                        span,
                        owner,
                        sol: None,
                    });
                }
                self.tr.wanted[i].sol = Some(Sol::Impl {
                    dict: found.dict,
                    ty: dict_ty,
                    subs,
                });
            }
            if !progress {
                break;
            }
        }
    }

    /// One trait at one type has one set of associated types: tie wanted `i`'s,
    /// at the variable `ty`, to those of whatever else asks the same.
    fn agree(&mut self, i: usize, tys: &[Type]) {
        let tr = self.tr.wanted[i].pred.tr;
        if self.tr.wanted[i].pred.assocs.is_empty() {
            return;
        }
        let mine = self.tr.wanted[i].pred.assocs.clone();
        let span = self.tr.wanted[i].span;
        let mut others: Vec<(Vec<Type>, Vec<Type>)> = Vec::new();
        for (j, w) in self.tr.wanted.iter().enumerate() {
            if j < i && w.pred.tr == tr {
                others.push((w.pred.tys.clone(), w.pred.assocs.clone()));
            }
        }
        for g in &self.tr.givens {
            if g.tr == tr {
                others.push((g.tys.clone(), g.assocs.clone()));
            }
        }
        for (of, theirs) in others {
            let of: Vec<Type> = of.iter().map(|t| self.arena.zonk(t)).collect();
            if of != tys {
                continue;
            }
            for (a, b) in mine.iter().zip(&theirs) {
                self.unify_at(span, a.clone(), b.clone());
            }
        }
    }

    /// The traits to go through to get from a dictionary of `from`, at `have`,
    /// to one of `to` at `want`: empty if it is that one, `None` if there is no
    /// way.
    fn super_path(
        &self,
        from: InternedString,
        have: &[Type],
        to: InternedString,
        want: &[Type],
    ) -> Option<Vec<(InternedString, Vec<usize>)>> {
        if from == to && have == want {
            return Some(Vec::new());
        }
        for (s, of) in &self.tr.shapes.get(&from)?.supers {
            let theirs: Vec<Type> = of.iter().filter_map(|k| have.get(*k).cloned()).collect();
            if let Some(mut rest) = self.super_path(*s, &theirs, to, want) {
                rest.insert(0, (*s, of.clone()));
                return Some(rest);
            }
        }
        None
    }

    /// Keep every variable something is still wanted of from generalizing here:
    /// a binding that takes no dictionaries cannot be general in it.
    pub(crate) fn hold_back_wanted(&mut self) {
        let level = self.arena.level;
        for i in 0..self.tr.wanted.len() {
            if self.tr.wanted[i].sol.is_some() {
                continue;
            }
            let pred = self.tr.wanted[i].pred.clone();
            for t in &pred.tys {
                self.arena.hold_back(t, level);
            }
            for a in &pred.assocs {
                self.arena.hold_back(a, level);
            }
        }
    }

    /// The top-level bindings `members` have been inferred, together: decide
    /// what each asks of its caller -- its `where`, over variables not yet
    /// quantified -- and answer what their bodies wanted of those variables.
    ///
    /// `dictionaries` is whether they may ask anything: only functions do.
    pub(crate) fn close_binding(
        &mut self,
        started: usize,
        members: &[(VarId, Type)],
        dictionaries: bool,
    ) -> HashMap<VarId, Vec<Pred>> {
        let level = self.arena.level;
        let givens = std::mem::take(&mut self.tr.givens);
        let signed = members
            .iter()
            .any(|(v, _)| self.sigs.contains_key(v) || self.tr.method_sigs.contains_key(v));
        self.tr.group.clear();
        if !dictionaries {
            self.hold_back_wanted();
            self.tr.mentions.clear();
            return HashMap::new();
        }

        // The group's `where`: what was given, then what turned out wanted.
        let mut all: Vec<Pred> = givens;
        let mut pending: Vec<(usize, usize, Vec<(InternedString, Vec<usize>)>)> = Vec::new();
        for i in started..self.tr.wanted.len() {
            if self.tr.wanted[i].sol.is_some() {
                continue;
            }
            let pred = self.tr.wanted[i].pred.clone();
            let span = self.tr.wanted[i].span;
            let want: Vec<Type> = pred.tys.iter().map(|t| self.arena.zonk(t)).collect();
            if want.iter().any(|t| t.references_error()) {
                continue;
            }
            let mut found = None;
            for (j, have) in all.iter().enumerate() {
                let theirs: Vec<Type> = have.tys.iter().map(|t| self.arena.zonk(t)).collect();
                if let Some(path) = self.super_path(have.tr, &theirs, pred.tr, &want) {
                    found = Some((j, path));
                    break;
                }
            }
            // Is any variable of it this binding's to generalize?
            let mut free = Vec::new();
            for t in &want {
                self.arena.free_vars(t, &mut free);
            }
            let ours = free.iter().any(|v| self.arena.slot_level(*v) > level);
            match found {
                Some((j, path)) => {
                    if path.is_empty() {
                        let theirs = all[j].assocs.clone();
                        for (a, b) in pred.assocs.iter().zip(&theirs) {
                            self.unify_at(span, a.clone(), b.clone());
                        }
                    }
                    pending.push((i, j, path));
                }
                // Variables from further out: not this binding's to ask for.
                None if !ours => {}
                None if signed => {
                    self.trait_error(
                        format!(
                            "this needs `{} {}`, which the signature does not ask for",
                            hir::spelling(&pred.tr),
                            show_all(&want)
                        ),
                        &format!("add `where {} …` to the signature", hir::spelling(&pred.tr)),
                        span,
                    );
                    self.tr.wanted[i].sol = Some(Sol::Failed);
                }
                None => {
                    pending.push((i, all.len(), Vec::new()));
                    all.push(pred);
                }
            }
        }

        // Each member asks for what its own type can settle: a trait of a
        // variable its caller could never choose is nobody's to supply.
        let mut lists: HashMap<VarId, Vec<usize>> = HashMap::new();
        for (vid, ty) in members {
            let mut settled = Vec::new();
            let z = self.arena.zonk(ty);
            self.arena.free_vars(&z, &mut settled);
            let mut mine: Vec<usize> = Vec::new();
            loop {
                let before = mine.len();
                for (j, p) in all.iter().enumerate() {
                    if mine.contains(&j) {
                        continue;
                    }
                    let mut of = Vec::new();
                    for t in &p.tys {
                        let pz = self.arena.zonk(t);
                        self.arena.free_vars(&pz, &mut of);
                    }
                    if of.iter().all(|v| settled.contains(v)) {
                        mine.push(j);
                        // Which settles its associated types in turn.
                        for a in &p.assocs {
                            let az = self.arena.zonk(a);
                            self.arena.free_vars(&az, &mut settled);
                        }
                    }
                }
                if mine.len() == before {
                    break;
                }
            }
            mine.sort_unstable();
            lists.insert(*vid, mine);
        }
        let only = (members.len() == 1).then(|| members[0].0);

        // A wanted is answered by its own function's parameter.
        for (i, j, path) in pending {
            let owner = self.tr.wanted[i].owner.or(only);
            let place = owner
                .and_then(|o| lists.get(&o))
                .and_then(|mine| mine.iter().position(|k| *k == j));
            self.tr.wanted[i].sol = Some(match place {
                Some(at) => Sol::Given(at, all[j].tys.clone(), path),
                None => {
                    let pred = self.tr.wanted[i].pred.clone();
                    let span = self.tr.wanted[i].span;
                    self.ambiguous(&pred, span);
                    Sol::Failed
                }
            });
        }

        // A member mentioned from inside the group was monomorphic there, so
        // nothing was wanted at the mention: it is handed the mentioning
        // function's own dictionaries, trait for trait.
        for (node, callee, from, span) in std::mem::take(&mut self.tr.mentions) {
            let (Some(theirs), Some(ours)) = (
                lists.get(&callee),
                from.or(only).and_then(|f| lists.get(&f)),
            ) else {
                continue;
            };
            let evidence: Vec<Evidence> = theirs
                .iter()
                .map(|j| match ours.iter().position(|k| k == j) {
                    Some(at) => Evidence::Given(at),
                    None => Evidence::Missing,
                })
                .collect();
            if evidence.contains(&Evidence::Missing) {
                let pred = all[theirs[0]].clone();
                self.ambiguous(&pred, span);
            }
            if !evidence.is_empty() {
                self.tr.fixed.insert(node, evidence);
            }
        }

        lists
            .into_iter()
            .map(|(v, mine)| (v, mine.into_iter().map(|j| all[j].clone()).collect()))
            .collect()
    }

    fn ambiguous(&mut self, pred: &Pred, span: Span) {
        let tys: Vec<Type> = pred.tys.iter().map(|t| self.arena.zonk(t)).collect();
        if tys.iter().any(|t| t.references_error()) {
            return;
        }
        self.trait_error(
            format!(
                "cannot tell which `impl {}` is meant: nothing here says what type `{}` is",
                hir::spelling(&pred.tr),
                show_all(&tys)
            ),
            "annotate a type to choose one",
            span,
        );
    }

    /// The unit is done: whatever is still wanted of an unknown type is an
    /// ambiguity, and everything else becomes the evidence lowering reads.
    pub(crate) fn finish_wanted(&mut self) -> HashMap<NodeId, Vec<Evidence>> {
        self.solve_wanted();
        for i in 0..self.tr.wanted.len() {
            if self.tr.wanted[i].sol.is_none() {
                let (pred, span) = (self.tr.wanted[i].pred.clone(), self.tr.wanted[i].span);
                self.ambiguous(&pred, span);
                self.tr.wanted[i].sol = Some(Sol::Failed);
            }
        }
        let mut out = std::mem::take(&mut self.tr.fixed);
        let nodes: Vec<(NodeId, Vec<usize>)> = self
            .tr
            .at_node
            .iter()
            .map(|(n, v)| (*n, v.clone()))
            .collect();
        for (node, at) in nodes {
            let evidence = at.into_iter().map(|i| self.evidence_of(i)).collect();
            out.insert(node, evidence);
        }
        out
    }

    fn evidence_of(&mut self, i: usize) -> Evidence {
        match self.tr.wanted[i].sol.clone() {
            None | Some(Sol::Failed) => Evidence::Missing,
            Some(Sol::Given(at, given, path)) => {
                // A required trait is of a selection of its requirer's types.
                let mut of: Vec<Type> = given.iter().map(|t| self.arena.zonk(t)).collect();
                let mut e = Evidence::Given(at);
                for (s, idxs) in path {
                    of = idxs.iter().filter_map(|k| of.get(*k).cloned()).collect();
                    e = Evidence::Super {
                        of: Box::new(e),
                        label: super_label(s),
                        ty: Type::Con(s, of.clone()),
                    };
                }
                e
            }
            Some(Sol::Impl { dict, ty, subs }) => Evidence::Impl {
                dict,
                ty: self.arena.zonk(&ty),
                args: subs.into_iter().map(|j| self.evidence_of(j)).collect(),
            },
        }
    }

    // --- associated types in written types ------------------------------------

    /// `ty` with every `Elem τ` replaced by a quantifier, a new one for each
    /// associated type at each type, remembered in `table`.
    fn lift_assocs(
        &self,
        ty: &Type,
        table: &mut Vec<(InternedString, Vec<Type>, u32)>,
        quant: &mut Vec<VarKind>,
    ) -> Type {
        let mut go = |t: &Type| self.lift_assocs(t, table, quant);
        match ty {
            Type::Con(name, args) if self.tr.assoc_of.contains_key(name) => {
                let of: Vec<Type> = args.iter().map(&mut go).collect();
                Type::Bound(assoc_var(*name, &of, table, quant))
            }
            Type::Con(name, args) => Type::Con(*name, args.iter().map(go).collect()),
            Type::Tuple(items) => Type::Tuple(items.iter().map(go).collect()),
            Type::Fun(params, ret, eff) => Type::Fun(
                params.iter().map(&mut go).collect(),
                Box::new(go(ret)),
                Box::new(go(eff)),
            ),
            Type::Record(row) => Type::Record(Box::new(go(row))),
            Type::RowExtend(l, field, rest) => {
                Type::RowExtend(*l, Box::new(go(field)), Box::new(go(rest)))
            }
            Type::Var(_) | Type::Bound(_) | Type::RowEmpty | Type::Error => ty.clone(),
        }
    }

    /// `tr` of `ty`, its associated types the quantifiers `table` has for
    /// them, or new ones.
    fn pred_over(
        &self,
        tr: InternedString,
        tys: Vec<Type>,
        table: &mut Vec<(InternedString, Vec<Type>, u32)>,
        quant: &mut Vec<VarKind>,
    ) -> Pred {
        let names = self
            .tr
            .shapes
            .get(&tr)
            .map(|s| s.assocs.clone())
            .unwrap_or_default();
        let assocs = names
            .into_iter()
            .map(|a| Type::Bound(assoc_var(a, &tys, table, quant)))
            .collect();
        Pred { tr, tys, assocs }
    }

    /// A written type and its `where`, as a scheme: every variable quantified,
    /// and one more for each associated type it mentions -- whose trait it
    /// asks for whether or not the `where` says so, since `Elem f` means
    /// nothing unless `f` is a `Container`.
    pub(crate) fn bounded_scheme(&self, t: &hir::LTypeExpr, bounds: &[hir::Bound]) -> Scheme {
        let mut vars = HashMap::new();
        collect_tyvars(t, &mut vars);
        for b in bounds {
            b.tys.iter().for_each(|t| collect_tyvars(t, &mut vars));
        }
        let raw = ty_of(t, &vars, &self.aliases);
        let mut quant = vec![VarKind::Type; vars.len()];
        mark_effect_vars(&raw, &mut quant);
        let mut table = Vec::new();
        let ty = self.lift_assocs(&raw, &mut table, &mut quant);
        let mut preds: Vec<Pred> = Vec::new();
        for b in bounds {
            let of: Vec<Type> = b
                .tys
                .iter()
                .map(|t| {
                    let of = ty_of(t, &vars, &self.aliases);
                    self.lift_assocs(&of, &mut table, &mut quant)
                })
                .collect();
            preds.push(self.pred_over(*b.tr.value(), of, &mut table, &mut quant));
        }
        let mut k = 0;
        while k < table.len() {
            let (assoc, of, _) = table[k].clone();
            k += 1;
            let Some((tr, _)) = self.tr.assoc_of.get(&assoc).copied() else {
                continue;
            };
            if !preds.iter().any(|p| p.tr == tr && p.tys == of) {
                preds.push(self.pred_over(tr, of, &mut table, &mut quant));
            }
        }
        Scheme { quant, preds, ty }
    }

    /// An annotation's type, after its variables have been made fresh: every
    /// `Elem τ` a fresh variable, wanted of `τ`'s `impl`.
    pub(crate) fn lift_assocs_wanted(&mut self, ty: Type, span: Span) -> Type {
        let mut table = Vec::new();
        let mut quant = Vec::new();
        let lifted = self.lift_assocs(&ty, &mut table, &mut quant);
        if table.is_empty() {
            return ty;
        }
        let mut preds: Vec<Pred> = Vec::new();
        let mut k = 0;
        while k < table.len() {
            let (assoc, of, _) = table[k].clone();
            k += 1;
            let Some((tr, _)) = self.tr.assoc_of.get(&assoc).copied() else {
                continue;
            };
            if !preds.iter().any(|p| p.tr == tr && p.tys == of) {
                preds.push(self.pred_over(tr, of, &mut table, &mut quant));
            }
        }
        let fresh: Vec<Type> = quant.iter().map(|k| self.arena.fresh_of(*k)).collect();
        let owner = self.tr.member;
        for p in preds {
            self.tr.wanted.push(Wanted {
                pred: Pred {
                    tr: p.tr,
                    tys: p
                        .tys
                        .iter()
                        .map(|t| Arena::subst_bound(t, &fresh))
                        .collect(),
                    assocs: p
                        .assocs
                        .iter()
                        .map(|a| Arena::subst_bound(a, &fresh))
                        .collect(),
                },
                span,
                owner,
                sol: None,
            });
        }
        Arena::subst_bound(&lifted, &fresh)
    }

    // --- declarations -----------------------------------------------------------

    /// A `trait`: its dictionary's type and constructor, and its methods'
    /// schemes.
    pub(crate) fn register_trait(&mut self, td: &hir::TraitDecl) {
        let np = td.params.len();
        let n = td.assocs.len();
        for (s, _) in &td.supers {
            if self
                .tr
                .shapes
                .get(s.value())
                .is_some_and(|shape| !shape.assocs.is_empty())
            {
                self.trait_error(
                    format!(
                        "`{}` has associated types, and a trait cannot require one that does yet",
                        hir::spelling(s.value())
                    ),
                    "ask for it in a `where` on the functions that need both",
                    s.span,
                );
            }
        }
        let mut labels: Vec<InternedString> = td
            .supers
            .iter()
            .map(|(s, _)| super_label(*s.value()))
            .collect();
        labels.extend(td.methods.iter().map(|m| m.name));
        let shape = TraitShape {
            params: np,
            supers: td
                .supers
                .iter()
                .map(|(s, of)| (*s.value(), of.clone()))
                .collect(),
            assocs: td.assocs.iter().map(|a| *a.value()).collect(),
            methods: td
                .methods
                .iter()
                .map(|m| (m.name, *m.var.value(), m.default.as_ref().map(|d| d.var)))
                .collect(),
            dict: td.dict,
            labels,
        };
        for (k, a) in td.assocs.iter().enumerate() {
            self.tr.assoc_of.insert(*a.value(), (td.name, k));
        }
        self.tr.shapes.insert(td.name, shape);

        // The dictionary: over the trait's parameters, then its associated
        // types.
        let head_args: Vec<Type> = (0..(np + n) as u32).map(Type::Bound).collect();
        let head = Type::Con(td.name, head_args.clone());
        let mut fields: Vec<(Option<InternedString>, Type)> = td
            .supers
            .iter()
            .map(|(s, of)| {
                (
                    Some(super_label(*s.value())),
                    Type::Con(
                        *s.value(),
                        of.iter().map(|k| Type::Bound(*k as u32)).collect(),
                    ),
                )
            })
            .collect();

        for m in &td.methods {
            let mut vars: HashMap<VarId, u32> = HashMap::new();
            for (i, p) in td.params.iter().enumerate() {
                vars.insert(*p.value(), i as u32);
            }
            let mut others = HashMap::new();
            collect_tyvars(&m.ty, &mut others);
            let mut extra: Vec<(VarId, u32)> = others
                .into_iter()
                .filter(|(v, _)| !vars.contains_key(v))
                .collect();
            extra.sort_by_key(|(_, i)| *i);
            for (j, (v, _)) in extra.iter().enumerate() {
                vars.insert(*v, (np + n + j) as u32);
            }
            let raw = ty_of(&m.ty, &vars, &self.aliases);
            let mut quant = vec![VarKind::Type; np + n + extra.len()];
            mark_effect_vars(&raw, &mut quant);
            if quant[np + n..].iter().any(|k| *k != VarKind::Effect) {
                self.trait_error(
                    format!(
                        "`{}` has a type variable of its own, which a trait's method cannot yet: \
                         only the trait's parameters, its associated types, and effects",
                        m.name
                    ),
                    "make it a function with a `where` instead, over a method that is not generic",
                    m.ty.span,
                );
            }
            // `Elem a` is the trait's own associated type: its quantifier.
            let of_params: Vec<Type> = (0..np as u32).map(Type::Bound).collect();
            let mut table: Vec<(InternedString, Vec<Type>, u32)> = td
                .assocs
                .iter()
                .enumerate()
                .map(|(k, a)| (*a.value(), of_params.clone(), (np + k) as u32))
                .collect();
            let known = table.len();
            let ty = self.lift_assocs(&raw, &mut table, &mut quant);
            if table.len() > known {
                self.trait_error(
                    format!(
                        "`{}` mentions an associated type of something other than the trait's parameters",
                        m.name
                    ),
                    "only the trait's own associated types, of its own parameters",
                    m.ty.span,
                );
            }
            // In the dictionary the method's effects are whatever the `impl`'s
            // are: a field cannot be general, and nothing reads an effect.
            let field = Arena::subst_bound(
                &ty,
                &(0..quant.len())
                    .map(|i| {
                        if i < np + n {
                            Type::Bound(i as u32)
                        } else {
                            Type::RowEmpty
                        }
                    })
                    .collect::<Vec<_>>(),
            );
            fields.push((Some(m.name), field));
            let scheme = Scheme {
                quant,
                preds: vec![Pred {
                    tr: td.name,
                    tys: head_args[..np].to_vec(),
                    assocs: head_args[np..].to_vec(),
                }],
                ty,
            };
            self.tr.methods.insert((td.name, m.name), scheme.clone());
            if let Some(d) = &m.default {
                self.env.insert(d.var, scheme.clone());
                self.tr
                    .method_sigs
                    .insert(d.var, (scheme.clone(), m.ty.span));
            }
            self.env.insert(*m.var.value(), scheme);
        }
        let quant = vec![VarKind::Type; np + n];
        self.record_ctor(td.name, td.dict, &quant, &head, &fields);
    }

    /// An `impl`: the scheme of its dictionary, found from here on by its
    /// trait and its types' constructors.
    pub(crate) fn register_impl(&mut self, id: &hir::ImplDecl) {
        let tr = *id.tr.value();
        let Some(shape) = self.tr.shapes.get(&tr).cloned() else {
            return;
        };
        let mut vars = HashMap::new();
        for t in &id.tys {
            collect_tyvars(t, &mut vars);
        }
        for b in &id.context {
            b.tys.iter().for_each(|t| collect_tyvars(t, &mut vars));
        }
        let heads: Vec<Type> = id
            .tys
            .iter()
            .map(|t| ty_of(t, &vars, &self.aliases))
            .collect();
        if heads.iter().any(|h| h.references_error()) {
            return;
        }
        // Each a constructor applied to distinct variables, so that finding
        // the `impl` for some types is looking up their constructors.
        let plain = heads.iter().all(|head| {
            let params: Vec<&Type> = match head {
                Type::Con(_, args) | Type::Tuple(args) => args.iter().collect(),
                _ => return false,
            };
            params
                .iter()
                .enumerate()
                .all(|(i, p)| matches!(p, Type::Bound(_)) && !params[..i].contains(p))
        });
        let Some(key) = heads_key(&heads).filter(|_| plain) else {
            self.trait_error(
                format!(
                    "`impl {} {}`: an `impl` is for a type constructor applied to distinct variables",
                    hir::spelling(&tr),
                    show_all(&heads)
                ),
                "like `Int`, `Maybe a` or `(a, b)`",
                id.tr.span,
            );
            return;
        };
        let mut quant = vec![VarKind::Type; vars.len()];
        let mut table = Vec::new();
        let mut preds = Vec::new();
        for b in &id.context {
            let of: Vec<Type> = b
                .tys
                .iter()
                .map(|t| {
                    let of = ty_of(t, &vars, &self.aliases);
                    self.lift_assocs(&of, &mut table, &mut quant)
                })
                .collect();
            preds.push(self.pred_over(*b.tr.value(), of, &mut table, &mut quant));
        }
        let mut args = heads.clone();
        for a in &shape.assocs {
            let is = id
                .assocs
                .iter()
                .find(|(l, _)| l.value() == a)
                .map(|(_, t)| ty_of(t, &vars, &self.aliases))
                .unwrap_or(Type::Error);
            args.push(self.lift_assocs(&is, &mut table, &mut quant));
        }
        let scheme = Scheme {
            quant,
            preds,
            ty: Type::Con(tr, args),
        };
        let dict = *id.dict.value();
        match self.tr.impls.get(&(tr, key.clone())) {
            Some(prev) if prev.dict != dict => {
                self.trait_error(
                    format!(
                        "`{}` is already implemented for `{}`",
                        hir::spelling(&tr),
                        show_all(&heads)
                    ),
                    "a second `impl` for the same type",
                    id.tr.span,
                );
                return;
            }
            _ => {}
        }
        self.env.insert(dict, scheme.clone());
        self.tr.impls.insert((tr, key), ImplDef { dict, scheme });
    }

    /// Record the arena variables behind a scheme nothing generalized -- a
    /// method's, a dictionary's -- so that lowering can bind them.
    fn record_poly(&mut self, vid: VarId, scheme: &Scheme) {
        let vars: Vec<u32> = scheme
            .quant
            .iter()
            .map(|k| match self.arena.fresh_of(*k) {
                Type::Var(id) => id,
                _ => unreachable!("a fresh variable"),
            })
            .collect();
        // Never to be solved: they stand for the quantifiers and nothing else.
        self.generalized.insert(
            vid,
            Generalized {
                scheme: scheme.clone(),
                vars,
            },
        );
    }

    /// This unit's traits and `impl`s: their methods and dictionaries are its
    /// to define and to export.
    pub fn export_traits(&mut self, decls: &[hir::LDecl]) {
        for d in decls {
            match d.value() {
                hir::Decl::Trait(td) => {
                    for m in &td.methods {
                        let vid = *m.var.value();
                        if let Some(s) = self.env.get(&vid).cloned() {
                            self.record_poly(vid, &s);
                            self.exports.push(vid);
                        }
                    }
                }
                hir::Decl::Impl(id) => {
                    let vid = *id.dict.value();
                    if let Some(s) = self.env.get(&vid).cloned() {
                        self.record_poly(vid, &s);
                        self.exports.push(vid);
                    }
                }
                _ => {}
            }
        }
    }

    /// Infer the bodies inside this module's traits and `impl`s: each default,
    /// against its method's type; each `impl`'s methods, against the trait's
    /// types at the implementing type. After every module's own bindings, which
    /// those bodies may mention.
    pub fn infer_traits(&mut self, module: &hir::LModule) {
        for d in &module.value().decls {
            match d.value() {
                hir::Decl::Trait(td) => {
                    for m in &td.methods {
                        if let Some(hir::DefaultMethod {
                            body: Some(body), ..
                        }) = &m.default
                        {
                            self.infer_bind(body, true);
                        }
                    }
                }
                hir::Decl::Impl(id) => self.infer_impl(id),
                _ => {}
            }
        }
    }

    fn infer_impl(&mut self, id: &hir::ImplDecl) {
        let tr = *id.tr.value();
        let dict = *id.dict.value();
        let (Some(shape), Some(found)) = (
            self.tr.shapes.get(&tr).cloned(),
            self.env.get(&dict).cloned(),
        ) else {
            return;
        };
        let Type::Con(_, dict_args) = found.ty.clone() else {
            return;
        };
        // Each method is held to the trait's type for it, at this type: the
        // trait's parameter and associated types replaced, the `impl`'s own
        // variables and `where` in front, the method's effects after them.
        for (name, body) in &id.methods {
            let Some(method) = self.tr.methods.get(&(tr, *name)).cloned() else {
                continue;
            };
            let base = found.quant.len() as u32;
            let mut quant = found.quant.clone();
            let own = dict_args.len();
            let subst: Vec<Type> = (0..method.quant.len())
                .map(|i| {
                    if i < own {
                        dict_args[i].clone()
                    } else {
                        quant.push(method.quant[i]);
                        Type::Bound(base + (i - own) as u32)
                    }
                })
                .collect();
            let scheme = Scheme {
                quant,
                preds: found.preds.clone(),
                ty: Arena::subst_bound(&method.ty, &subst),
            };
            let vid = body.bound_vars()[0];
            self.tr.method_sigs.insert(vid, (scheme, id.tr.span));
            self.infer_bind(body, true);
        }

        // The dictionaries of the traits this one requires, of the same type,
        // from what this `impl` is given.
        if !shape.supers.is_empty() {
            let started = self.begin_binding();
            self.arena.enter_level();
            // Over the dictionary's own binders, not fresh variables: what is
            // found here is written into its body, in its terms. An `impl`
            // answering one of these is unified *into* them -- the left of
            // two variables is the one bound -- so they stay what they are.
            let binders: Vec<Type> = self
                .generalized
                .get(&dict)
                .map(|g| g.vars.iter().map(|v| Type::Var(*v)).collect())
                .unwrap_or_default();
            let (inst, given) = if binders.len() == found.quant.len() {
                let at = |t: &Type| Arena::subst_bound(t, &binders);
                (
                    at(&found.ty),
                    found
                        .preds
                        .iter()
                        .map(|p| Pred {
                            tr: p.tr,
                            tys: p.tys.iter().map(at).collect(),
                            assocs: p.assocs.iter().map(at).collect(),
                        })
                        .collect(),
                )
            } else {
                self.instantiate_with_preds(&found)
            };
            self.tr.givens = given;
            let of: Vec<Type> = match &inst {
                Type::Con(_, args) => args.clone(),
                _ => Vec::new(),
            };
            let at: Vec<usize> = shape
                .supers
                .iter()
                .map(|(s, idxs)| {
                    self.tr.wanted.push(Wanted {
                        pred: Pred {
                            tr: *s,
                            tys: idxs
                                .iter()
                                .map(|k| of.get(*k).cloned().unwrap_or(Type::Error))
                                .collect(),
                            assocs: Vec::new(),
                        },
                        span: id.tr.span,
                        owner: Some(dict),
                        sol: None,
                    });
                    self.tr.wanted.len() - 1
                })
                .collect();
            self.tr.at_node.insert(id.dict.id, at);
            self.solve_wanted();
            self.arena.exit_level();
            // Signed, by the `impl`'s own `where`: what is wanted of its
            // variables has to be in it.
            self.tr
                .method_sigs
                .insert(dict, (found.clone(), id.tr.span));
            self.close_binding(started, &[(dict, inst)], true);
            self.tr.method_sigs.remove(&dict);
        }
    }
}

/// The quantifier standing for associated type `assoc` of `of`.
fn assoc_var(
    assoc: InternedString,
    of: &[Type],
    table: &mut Vec<(InternedString, Vec<Type>, u32)>,
    quant: &mut Vec<VarKind>,
) -> u32 {
    if let Some((_, _, i)) = table.iter().find(|(a, t, _)| *a == assoc && t == of) {
        return *i;
    }
    quant.push(VarKind::Type);
    let i = (quant.len() - 1) as u32;
    table.push((assoc, of.to_vec(), i));
    i
}

/// What an `impl` of a trait of these types is found by: the outermost
/// constructor of each, if every one of them is known that far.
fn heads_key(tys: &[Type]) -> Option<String> {
    let heads: Option<Vec<String>> = tys.iter().map(head_key).collect();
    Some(heads?.join(" "))
}

/// Several types as a program writes them after a trait's name.
fn show_all(tys: &[Type]) -> String {
    tys.iter()
        .map(|t| match t {
            Type::Con(_, args) if !args.is_empty() => format!("({})", show(t)),
            Type::Fun(..) => format!("({})", show(t)),
            _ => show(t),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

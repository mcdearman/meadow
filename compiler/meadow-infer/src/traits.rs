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
//!
//! A trait that requires another **inherits** its associated types. Given
//! `trait Visual s <: Stream s`, `Token s` is as much `Visual`'s as
//! `Stream`'s: its dictionary is over one more variable for it, its methods
//! may mention it, and a `Visual s` given to a function brings `Stream s` with
//! the same `Token s`. An `impl Visual T` does not say what `Token T` is -- its
//! `Stream T` does -- so it asks for its `Stream T` as if its `where` had, and
//! answering that is what settles the inherited types wherever it is used.

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
    /// Its associated types: its own, then those it inherits from the traits
    /// it requires. A predicate of the trait carries one type for each.
    pub assocs: Vec<InternedString>,
    /// Which of its parameters each associated type is of: all of them for
    /// its own, the requiring trait's selection for an inherited one.
    pub assoc_params: Vec<Vec<usize>>,
    /// How many of `assocs` are its own.
    pub own_assocs: usize,
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
    /// are selected from, and its associated types, which a required trait's
    /// are among.
    Given(
        usize,
        Vec<Type>,
        Vec<Type>,
        Vec<(InternedString, Vec<usize>)>,
    ),
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
    /// The trait of the given a wanted is answered from, by the wanted.
    given_of: HashMap<usize, InternedString>,
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
                let found = self
                    .tr
                    .impls
                    .get(&(tr, key))
                    .or_else(|| self.tr.impls.get(&(tr, blanket_key(tys.len()))))
                    .cloned();
                let Some(found) = found else {
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
                self.answer_with(i, tys, &found);
            }
            if !progress {
                break;
            }
        }
    }

    /// Answer wanted `i`, at `tys`, with the `impl` `found`: its dictionary,
    /// applied to what that `impl`'s own `where` wants in turn.
    fn answer_with(&mut self, i: usize, tys: Vec<Type>, found: &ImplDef) {
        let (tr, span) = (self.tr.wanted[i].pred.tr, self.tr.wanted[i].span);
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

    /// Wanted `i` is of a type nothing will ever say: `[;] == [;]`, or `1 + 2`
    /// in a binding that is not a function. Answer it the way it has to mean
    /// something -- with the trait's `impl` for every type, if it has one, or
    /// else at `Int`, if it is an integer literal's type or an arithmetic
    /// trait's, and `Int` implements the trait -- and say whether that worked.
    fn default_wanted(&mut self, i: usize) -> bool {
        let tr = self.tr.wanted[i].pred.tr;
        let tys: Vec<Type> = self.tr.wanted[i]
            .pred
            .tys
            .clone()
            .iter()
            .map(|t| self.arena.zonk(t))
            .collect();
        // Settled by now, by another one defaulted before it: answered as
        // any wanted of a known type is.
        if let Some(key) = heads_key(&tys)
            && let Some(found) = self.tr.impls.get(&(tr, key)).cloned()
        {
            self.answer_with(i, tys, &found);
            return true;
        }
        if let Some(found) = self.tr.impls.get(&(tr, blanket_key(tys.len()))).cloned() {
            self.answer_with(i, tys, &found);
            return true;
        }
        // A float literal's type, which would be `Float` by the end anyway.
        if let [Type::Var(v)] = tys.as_slice()
            && matches!(self.arena.slot_kind(*v), VarKind::Frac)
        {
            if let Some(found) = self.tr.impls.get(&(tr, "Float".to_string())).cloned() {
                let span = self.tr.wanted[i].span;
                self.unify_at(span, tys[0].clone(), Type::float());
                self.answer_with(i, vec![Type::float()], &found);
                return true;
            }
            return false;
        }
        // An integer literal's type, which would be `Int` by the end anyway,
        // or what an arithmetic operator was used at (`hir::NUMERIC_TRAITS`).
        // Not any variable: `make ()` thrown away is not a request for `Int`.
        if let [Type::Var(v)] = tys.as_slice()
            && (matches!(self.arena.slot_kind(*v), VarKind::Num) || hir::is_numeric_trait(&tr))
            && let Some(found) = self.tr.impls.get(&(tr, "Int".to_string())).cloned()
        {
            let span = self.tr.wanted[i].span;
            self.unify_at(span, tys[0].clone(), Type::int());
            self.answer_with(i, vec![Type::int()], &found);
            return true;
        }
        false
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

    /// The associated types of `sup`, required by `tr` of the parameters
    /// `idxs`, out of `tr`'s own: `assocs` is a predicate of `tr`'s.
    fn super_assocs(
        &self,
        tr: InternedString,
        assocs: &[Type],
        sup: InternedString,
        idxs: &[usize],
    ) -> Vec<Type> {
        let (Some(me), Some(them)) = (self.tr.shapes.get(&tr), self.tr.shapes.get(&sup)) else {
            return Vec::new();
        };
        them.assocs
            .iter()
            .zip(&them.assoc_params)
            .map(|(a, ps)| {
                let mapped: Vec<usize> = ps.iter().filter_map(|p| idxs.get(*p).copied()).collect();
                me.assocs
                    .iter()
                    .zip(&me.assoc_params)
                    .position(|(b, qs)| b == a && *qs == mapped)
                    .and_then(|k| assocs.get(k).cloned())
                    .unwrap_or(Type::Error)
            })
            .collect()
    }

    /// Associated type `assoc` of trait `owner` at the types `of` -- written
    /// over an `impl`'s own quantifiers -- as the `impl` of `owner` for them
    /// says, if it is known and says it in terms of `of` alone.
    fn known_assoc(
        &self,
        owner: InternedString,
        assoc: InternedString,
        of: &[Type],
    ) -> Option<Type> {
        let found = self.tr.impls.get(&(owner, heads_key(of)?))?;
        let shape = self.tr.shapes.get(&owner)?;
        let Type::Con(_, theirs) = &found.scheme.ty else {
            return None;
        };
        let k = shape.assocs[..shape.own_assocs]
            .iter()
            .position(|x| *x == assoc)?;
        let mut map = HashMap::new();
        for (pattern, t) in theirs.get(..shape.params)?.iter().zip(of) {
            match_bound(pattern, t, &mut map)?;
        }
        subst_map(theirs.get(shape.params + k)?, &map)
    }

    /// How many traits deep `tr`'s requirements go: 0 for one that requires
    /// none. Bounded, against a cycle a program wrote by mistake.
    pub(crate) fn trait_depth(&self, tr: InternedString, seen: usize) -> usize {
        if seen > 32 {
            return seen;
        }
        self.tr
            .shapes
            .get(&tr)
            .map(|s| {
                s.supers
                    .iter()
                    .map(|(sup, _)| 1 + self.trait_depth(*sup, seen + 1))
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0)
    }

    /// The trait of the given wanted `i` was answered from.
    fn given_trait(&self, i: usize) -> Option<InternedString> {
        self.tr.given_of.get(&i).copied()
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

        // The variables of the members' own types: what a signature speaks of.
        let mut signature_vars = Vec::new();
        for (_, ty) in members {
            let z = self.arena.zonk(ty);
            self.arena.free_vars(&z, &mut signature_vars);
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
                    // What the given says the associated types are, through
                    // every trait on the way to this one.
                    let mut tr = all[j].tr;
                    let mut theirs = all[j].assocs.clone();
                    for (s, idxs) in &path {
                        theirs = self.super_assocs(tr, &theirs, *s, idxs);
                        tr = *s;
                    }
                    for (a, b) in pred.assocs.iter().zip(&theirs) {
                        self.unify_at(span, a.clone(), b.clone());
                    }
                    self.tr.given_of.insert(i, all[j].tr);
                    pending.push((i, j, path));
                }
                // Variables from further out: not this binding's to ask for.
                None if !ours => {}
                None if signed => {
                    // A variable only the body has -- a literal's, `compare 0
                    // 0` -- is not the signature's to have asked for: it is
                    // defaulted, as it would be in a binding with none.
                    let local = !free.iter().any(|v| signature_vars.contains(v));
                    if local && self.default_wanted(i) {
                        continue;
                    }
                    self.trait_error(
                        format!(
                            "this needs `{} {}`, which the signature does not ask for",
                            hir::spelling(&pred.tr),
                            show_all(&want)
                        ),
                        &format!("add `{} … =>` to the signature", hir::spelling(&pred.tr)),
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
                Some(at) => Sol::Given(at, all[j].tys.clone(), all[j].assocs.clone(), path),
                None => {
                    self.tr.wanted[i].sol = None;
                    if self.default_wanted(i) {
                        continue;
                    }
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
        // What nothing said the type of: defaulted, one at a time, since each
        // one settled may settle others. What is left after that is ambiguous.
        let mut i = 0;
        while i < self.tr.wanted.len() {
            if self.tr.wanted[i].sol.is_none() && self.default_wanted(i) {
                self.solve_wanted();
            }
            i += 1;
        }
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
            Some(Sol::Given(at, given, assocs, path)) => {
                // A required trait is of a selection of its requirer's types,
                // and its associated types are some of its requirer's.
                let mut of: Vec<Type> = given.iter().map(|t| self.arena.zonk(t)).collect();
                let mut asc: Vec<Type> = assocs.iter().map(|t| self.arena.zonk(t)).collect();
                let mut tr = self.given_trait(i).unwrap_or(self.tr.wanted[i].pred.tr);
                let mut e = Evidence::Given(at);
                for (s, idxs) in path {
                    asc = self.super_assocs(tr, &asc, s, &idxs);
                    of = idxs.iter().filter_map(|k| of.get(*k).cloned()).collect();
                    let mut args = of.clone();
                    args.extend(asc.iter().cloned());
                    e = Evidence::Super {
                        of: Box::new(e),
                        label: super_label(s),
                        ty: Type::Con(s, args),
                    };
                    tr = s;
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
        let (names, params) = self
            .tr
            .shapes
            .get(&tr)
            .map(|s| (s.assocs.clone(), s.assoc_params.clone()))
            .unwrap_or_default();
        let assocs = names
            .into_iter()
            .zip(params)
            .map(|(a, ps)| {
                let of: Vec<Type> = ps.iter().filter_map(|p| tys.get(*p).cloned()).collect();
                Type::Bound(assoc_var(a, &of, table, quant))
            })
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
        // Its own associated types, then what it inherits from the traits it
        // requires, each at the selection of its parameters it is of.
        let mut assocs: Vec<InternedString> = td.assocs.iter().map(|a| *a.value()).collect();
        let mut assoc_params: Vec<Vec<usize>> = vec![(0..np).collect(); assocs.len()];
        for (s, idxs) in &td.supers {
            let Some(shape) = self.tr.shapes.get(s.value()) else {
                continue;
            };
            for (a, ps) in shape.assocs.iter().zip(&shape.assoc_params) {
                let mapped: Vec<usize> = ps.iter().filter_map(|p| idxs.get(*p).copied()).collect();
                let known = assocs
                    .iter()
                    .zip(&assoc_params)
                    .any(|(b, qs)| b == a && *qs == mapped);
                if !known {
                    assocs.push(*a);
                    assoc_params.push(mapped);
                }
            }
        }
        let n = assocs.len();
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
            assocs: assocs.clone(),
            assoc_params: assoc_params.clone(),
            own_assocs: td.assocs.len(),
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
                let mut args: Vec<Type> = of.iter().map(|k| Type::Bound(*k as u32)).collect();
                args.extend(self.super_assocs(td.name, &head_args[np..], *s.value(), of));
                (Some(super_label(*s.value())), Type::Con(*s.value(), args))
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
            // `Elem a` is the trait's own associated type, or one it inherits:
            // its quantifier.
            let mut table: Vec<(InternedString, Vec<Type>, u32)> = assocs
                .iter()
                .zip(&assoc_params)
                .enumerate()
                .map(|(k, (a, ps))| {
                    let of = ps.iter().map(|p| Type::Bound(*p as u32)).collect();
                    (*a, of, (np + k) as u32)
                })
                .collect();
            let known = table.len();
            let ty = self.lift_assocs(&raw, &mut table, &mut quant);
            if table.len() > known {
                self.trait_error(
                    format!(
                        "`{}` mentions an associated type of something other than the trait's parameters",
                        m.name
                    ),
                    "only the associated types of the trait's parameters, its own or those it requires",
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
        // Or every one a variable of its own: `impl Eq a`, the `impl` for any
        // type no other `impl` is for.
        let blanket = heads
            .iter()
            .enumerate()
            .all(|(i, h)| matches!(h, Type::Bound(_)) && !heads[..i].contains(h));
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
        let key = if blanket {
            Some(blanket_key(heads.len()))
        } else {
            heads_key(&heads).filter(|_| plain)
        };
        let Some(key) = key else {
            self.trait_error(
                format!(
                    "`impl {} {}`: an `impl` is for a type constructor applied to distinct variables",
                    hir::spelling(&tr),
                    show_all(&heads)
                ),
                "like `Int`, `Maybe a`, `(a, b)`, or `a` for every type",
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
        for a in &shape.assocs[..shape.own_assocs] {
            let is = id
                .assocs
                .iter()
                .find(|(l, _)| l.value() == a)
                .map(|(_, t)| ty_of(t, &vars, &self.aliases))
                .unwrap_or(Type::Error);
            args.push(self.lift_assocs(&is, &mut table, &mut quant));
        }
        // An inherited one is whatever the `impl` of the trait that declares it
        // says. When that `impl` is known already, it is read off it, so that
        // `impl Visual String`'s methods see `Token String` as `Char`; when it
        // is not, this `impl` asks for that one, as if its `where` had, and
        // answering that settles it wherever it is used.
        for (a, ps) in shape
            .assocs
            .iter()
            .zip(&shape.assoc_params)
            .skip(shape.own_assocs)
        {
            let of: Vec<Type> = ps.iter().filter_map(|p| heads.get(*p).cloned()).collect();
            let owner = self.tr.assoc_of.get(a).map(|(o, _)| *o);
            if let Some(known) = owner.and_then(|o| self.known_assoc(o, *a, &of)) {
                args.push(known);
                continue;
            }
            args.push(Type::Bound(assoc_var(*a, &of, &mut table, &mut quant)));
            if let Some(owner) = owner
                && !preds.iter().any(|p: &Pred| p.tr == owner && p.tys == of)
            {
                preds.push(self.pred_over(owner, of, &mut table, &mut quant));
            }
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
                            assocs: self.super_assocs(
                                tr,
                                of.get(shape.params..).unwrap_or(&[]),
                                *s,
                                idxs,
                            ),
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

/// Match an `impl`'s head, over its quantifiers, against types: what each
/// quantifier stands for, if the head fits.
fn match_bound(pattern: &Type, t: &Type, map: &mut HashMap<u32, Type>) -> Option<()> {
    match (pattern, t) {
        (Type::Bound(i), _) => match map.get(i) {
            Some(prev) => (prev == t).then_some(()),
            None => {
                map.insert(*i, t.clone());
                Some(())
            }
        },
        (Type::Con(a, xs), Type::Con(b, ys)) if a == b && xs.len() == ys.len() => xs
            .iter()
            .zip(ys)
            .try_for_each(|(x, y)| match_bound(x, y, map)),
        (Type::Tuple(xs), Type::Tuple(ys)) if xs.len() == ys.len() => xs
            .iter()
            .zip(ys)
            .try_for_each(|(x, y)| match_bound(x, y, map)),
        _ => (pattern == t).then_some(()),
    }
}

/// `ty` with its quantifiers replaced as `map` says; `None` if one is not
/// there.
fn subst_map(ty: &Type, map: &HashMap<u32, Type>) -> Option<Type> {
    let each = |ts: &[Type]| {
        ts.iter()
            .map(|t| subst_map(t, map))
            .collect::<Option<Vec<_>>>()
    };
    Some(match ty {
        Type::Bound(i) => map.get(i)?.clone(),
        Type::Con(n, args) => Type::Con(*n, each(args)?),
        Type::Tuple(items) => Type::Tuple(each(items)?),
        Type::Fun(ps, ret, eff) => Type::Fun(
            each(ps)?,
            Box::new(subst_map(ret, map)?),
            Box::new(subst_map(eff, map)?),
        ),
        Type::Record(row) => Type::Record(Box::new(subst_map(row, map)?)),
        Type::RowExtend(l, f, rest) => Type::RowExtend(
            *l,
            Box::new(subst_map(f, map)?),
            Box::new(subst_map(rest, map)?),
        ),
        Type::Var(_) | Type::RowEmpty | Type::Error => ty.clone(),
    })
}

/// What an `impl` of a trait of these types is found by: the outermost
/// constructor of each, if every one of them is known that far.
fn heads_key(tys: &[Type]) -> Option<String> {
    let heads: Option<Vec<String>> = tys.iter().map(head_key).collect();
    Some(heads?.join(" "))
}

/// What the `impl` of a trait of `n` types for every type is found by.
fn blanket_key(n: usize) -> String {
    vec!["_"; n].join(" ")
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

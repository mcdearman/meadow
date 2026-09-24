//! **Types as a search sees them**, and what it means for one to match another.
//!
//! The compiler's types carry what inference needs -- effect rows, canonical
//! names, meta variables. A search needs less, and needs it in a form that can
//! be written to a file and read back by a program that has never seen the
//! compiler's arena. So a signature is flattened into [`Sig`]: the traits it
//! asks for, its arguments in order, and its result, over a small [`Ty`] whose
//! names are spelled the way a person writes them.
//!
//! # Matching the way Hoogle does
//!
//! Somebody searching `[a] -> Int` wants `length`, whose type is exactly that,
//! but they would also take `sum : [Int] -> Int`, a little less. They do not
//! mind that `take` wants its count first when they asked for `[a] -> Int ->
//! [a]`, and they would rather see `foldl`, which needs one more argument than
//! they gave, than nothing at all. What they do not want is a function whose
//! result is not what they asked for, however well the arguments line up.
//!
//! So a match is a unification of the query with the declaration, their type
//! variables kept apart, under every way of pairing the query's arguments with
//! the declaration's -- and a match is then *priced*, cheapest first:
//!
//! | what happened                                            | cost |
//! | -------------------------------------------------------- | ---- |
//! | the same type, up to naming its variables                |    0 |
//! | the arguments came in another order                      |    1 |
//! | a variable of the declaration had to be a particular type |    1 |
//! | the declaration takes an argument the query did not give |    2 |
//! | the declaration forces two of the query's variables equal |    2 |
//! | a variable of the query had to be a particular type      |    3 |
//! | the declaration's result is only a variable              |    3 |
//!
//! That last one is dearest because it is the declaration being *less* general
//! than asked: somebody who wrote `a` did not say `Int`, and a function that
//! only works on `Int` may not do what they want. A declaration more general
//! than asked always does.
//!
//! The result has to unify or there is no match at all. Everything else is a
//! price, which is what lets a search half typed still show something useful.

use meadow_compiler::{hir, infer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A type, as simple as matching can make do with.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ty {
    /// A type variable, numbered within its signature.
    Var(u32),
    /// A named type applied to its arguments, spelled as written: `Int`,
    /// `Maybe a`, and `Vector a` for what is written `[a]`.
    Con(String, Vec<Ty>),
    /// A function taken or returned as a value -- `(a -> b)` in `map`'s type --
    /// its arguments flattened as a [`Sig`]'s are.
    Fun(Vec<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    /// A record, its fields sorted by label.
    Record(Vec<(String, Ty)>),
    /// Anything at all: `_` in a query, the part of a query not typed yet, or
    /// something the compiler could not say.
    Any,
}

/// A signature flattened for comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sig {
    /// The traits it asks of its variables, by name: `Show` for `Show a =>`.
    pub context: Vec<String>,
    pub args: Vec<Ty>,
    pub result: Ty,
}

// --- from the compiler -------------------------------------------------------

impl Sig {
    /// A top-level scheme, as a search compares it.
    pub fn of_scheme(s: &infer::Scheme) -> Sig {
        let (args, result) = flatten(&s.ty);
        Sig {
            context: s
                .preds
                .iter()
                .map(|p| hir::spelling(&p.tr).to_string())
                .collect(),
            args,
            result,
        }
    }
}

/// A curried function's arguments, and what is left when they are all given.
fn flatten(t: &infer::Type) -> (Vec<Ty>, Ty) {
    let mut args = Vec::new();
    let mut at = t;
    while let infer::Type::Fun(ps, r, _) = at {
        args.extend(ps.iter().map(ty_of));
        at = r;
    }
    (args, ty_of(at))
}

fn ty_of(t: &infer::Type) -> Ty {
    use infer::Type as T;
    match t {
        T::Bound(i) => Ty::Var(*i),
        // A meta variable a scheme never generalised. It is still a variable,
        // but must not be confused with the scheme's own.
        T::Var(v) => Ty::Var(META + v),
        T::Con(name, args) => Ty::Con(
            hir::spelling(name).to_string(),
            args.iter().map(ty_of).collect(),
        ),
        T::Fun(..) => {
            let (args, result) = flatten(t);
            Ty::Fun(args, Box::new(result))
        }
        T::Tuple(items) => Ty::Tuple(items.iter().map(ty_of).collect()),
        T::Record(row) => {
            let mut fields = Vec::new();
            let mut at = &**row;
            while let T::RowExtend(label, field, rest) = at {
                fields.push((label.to_string(), ty_of(field)));
                at = rest;
            }
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            Ty::Record(fields)
        }
        T::RowEmpty | T::RowExtend(..) | T::Error => Ty::Any,
    }
}

/// Where a scheme's unsolved meta variables are numbered from, clear of its
/// quantified ones.
const META: u32 = 1 << 16;

// --- a query, as somebody types it ------------------------------------------

/// Read a type the way it is written in a signature, and forgive whatever has
/// not been typed yet: `[a] -> ` is a function from `[a]` to anything, and
/// `Maybe` with nothing after it is `Maybe` of anything.
///
/// Takes what a signature can say -- `C a =>` contexts, `->`, applications,
/// `[a]` and `#[a]`, tuples, `()`, records, and `_` for anything -- and stops
/// at an effect's `!`, which a search does not compare.
pub fn parse(query: &str) -> Sig {
    let tokens = lex(query);
    let mut p = Parser {
        tokens,
        at: 0,
        vars: HashMap::new(),
    };
    let context = p.context();
    let body = p.arrow();
    let (args, result) = match body {
        Ty::Fun(args, result) => (args, *result),
        other => (Vec::new(), other),
    };
    Sig {
        context,
        args,
        result,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Arrow,
    FatArrow,
    Open,
    Close,
    OpenList,
    OpenArray,
    CloseList,
    OpenBrace,
    CloseBrace,
    Comma,
    Colon,
    Bang,
    Upper(String),
    Lower(String),
    Hole,
}

fn lex(s: &str) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match c {
            c if c.is_whitespace() => i += 1,
            '-' if next == Some('>') => {
                out.push(Tok::Arrow);
                i += 2;
            }
            '=' if next == Some('>') => {
                out.push(Tok::FatArrow);
                i += 2;
            }
            '#' if next == Some('[') => {
                out.push(Tok::OpenArray);
                i += 2;
            }
            '(' => (out.push(Tok::Open), i += 1).1,
            ')' => (out.push(Tok::Close), i += 1).1,
            '[' => (out.push(Tok::OpenList), i += 1).1,
            ']' => (out.push(Tok::CloseList), i += 1).1,
            '{' => (out.push(Tok::OpenBrace), i += 1).1,
            '}' => (out.push(Tok::CloseBrace), i += 1).1,
            ',' => (out.push(Tok::Comma), i += 1).1,
            ':' => (out.push(Tok::Colon), i += 1).1,
            '!' => (out.push(Tok::Bang), i += 1).1,
            c if c.is_alphanumeric() || c == '_' => {
                let start = i;
                while i < chars.len()
                    && (chars[i].is_alphanumeric()
                        || chars[i] == '_'
                        || chars[i] == '\''
                        || (chars[i] == '.' && chars.get(i + 1).is_some_and(|c| c.is_alphabetic())))
                {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                // `Std.Maybe.Maybe` is `Maybe`: a search matches spellings.
                let last = word.rsplit('.').next().unwrap_or(&word).to_string();
                out.push(if last == "_" {
                    Tok::Hole
                } else if last.starts_with(|c: char| c.is_uppercase()) {
                    Tok::Upper(last)
                } else {
                    Tok::Lower(last)
                });
            }
            // Anything else is not something a type is written with; skipping
            // it is kinder than refusing the whole query.
            _ => i += 1,
        }
    }
    out
}

struct Parser {
    tokens: Vec<Tok>,
    at: usize,
    /// Each variable's number, in the order the query first mentions it.
    vars: HashMap<String, u32>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// `Show a =>` or `(Show a, Ord b) =>`, if the query starts with one: the
    /// names of the traits.
    fn context(&mut self) -> Vec<String> {
        let Some(fat) = self.tokens.iter().position(|t| *t == Tok::FatArrow) else {
            return Vec::new();
        };
        let names = self.tokens[..fat]
            .iter()
            .filter_map(|t| match t {
                Tok::Upper(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        self.at = fat + 1;
        names
    }

    /// A type, arrows and all: right-associative, and flattened as it goes.
    fn arrow(&mut self) -> Ty {
        let first = self.app();
        if !self.eat(&Tok::Arrow) {
            return first;
        }
        // Nothing after the arrow yet is a result not typed yet.
        let rest = if self.starts_atom() {
            self.arrow()
        } else {
            Ty::Any
        };
        match rest {
            Ty::Fun(mut args, result) => {
                args.insert(0, first);
                Ty::Fun(args, result)
            }
            result => Ty::Fun(vec![first], Box::new(result)),
        }
    }

    fn starts_atom(&self) -> bool {
        matches!(
            self.peek(),
            Some(
                Tok::Upper(_)
                    | Tok::Lower(_)
                    | Tok::Hole
                    | Tok::Open
                    | Tok::OpenList
                    | Tok::OpenArray
                    | Tok::OpenBrace
            )
        )
    }

    /// `Maybe a`, or anything else an atom starts.
    fn app(&mut self) -> Ty {
        if !self.starts_atom() {
            return Ty::Any;
        }
        let head = self.atom();
        let mut args = Vec::new();
        while self.starts_atom() {
            args.push(self.atom());
        }
        match head {
            Ty::Con(name, mut already) if already.is_empty() => {
                already.append(&mut args);
                Ty::Con(name, already)
            }
            // A variable applied to something, `f a`, is more than this
            // search compares: it matches anything.
            _ if !args.is_empty() => Ty::Any,
            head => head,
        }
    }

    fn atom(&mut self) -> Ty {
        let Some(t) = self.peek().cloned() else {
            return Ty::Any;
        };
        self.at += 1;
        match t {
            Tok::Upper(name) => Ty::Con(name, Vec::new()),
            Tok::Lower(name) => {
                let next = self.vars.len() as u32;
                Ty::Var(*self.vars.entry(name).or_insert(next))
            }
            Tok::Hole => Ty::Any,
            Tok::Open => {
                if self.eat(&Tok::Close) {
                    return Ty::Con("Unit".into(), Vec::new());
                }
                let mut items = vec![self.arrow()];
                while self.eat(&Tok::Comma) {
                    items.push(self.arrow());
                }
                self.eat(&Tok::Close);
                if items.len() == 1 {
                    items.pop().unwrap_or(Ty::Any)
                } else {
                    Ty::Tuple(items)
                }
            }
            Tok::OpenList => {
                let item = self.arrow();
                self.eat(&Tok::CloseList);
                Ty::Con("Vector".into(), vec![item])
            }
            Tok::OpenArray => {
                let item = self.arrow();
                self.eat(&Tok::CloseList);
                Ty::Con("Array".into(), vec![item])
            }
            Tok::OpenBrace => {
                let mut fields = Vec::new();
                while let Some(Tok::Lower(label)) = self.peek().cloned() {
                    self.at += 1;
                    self.eat(&Tok::Colon);
                    fields.push((label, self.arrow()));
                    if !self.eat(&Tok::Comma) {
                        break;
                    }
                }
                self.eat(&Tok::CloseBrace);
                fields.sort_by(|a, b| a.0.cmp(&b.0));
                Ty::Record(fields)
            }
            // Punctuation where a type should be: `!` starts an effect, which
            // is the end of what is compared; anything else is a slip.
            _ => {
                self.at = self.tokens.len();
                Ty::Any
            }
        }
    }
}

// --- matching ------------------------------------------------------------------

/// Where a declaration's variables are numbered from while it is matched, so
/// that they and the query's are never the same variable by accident.
const DECL: u32 = 1 << 24;

/// What it costs to read `decl` as an answer to `query`: `None` if it is not
/// one, and the cheaper the better otherwise. See the module docs for the
/// prices.
pub fn cost(query: &Sig, decl: &Sig) -> Option<u32> {
    let m = query.args.len();
    let n = decl.args.len();
    // More arguments than the declaration takes, it cannot be given.
    if m > n {
        return None;
    }
    let result = rename(&decl.result);
    let args: Vec<Ty> = decl.args.iter().map(rename).collect();

    // The result is what cannot be negotiated, and trying it first rules out
    // nearly every declaration before any argument is looked at.
    let mut base = Subst::default();
    if !base.unify(&query.result, &result) {
        return None;
    }

    let mut best: Option<u32> = None;
    for pick in pairings(m, n) {
        let mut s = base.clone();
        if !query
            .args
            .iter()
            .zip(&pick)
            .all(|(q, &j)| s.unify(q, &args[j]))
        {
            continue;
        }
        let mut c = s.price(query, decl);
        // A declaration taking more than was asked still answers, a little
        // less well; one whose type is only a result answers a query that is
        // only a result, and taking arguments is then the smaller cost.
        let extra = (n - m) as u32;
        c += extra * if m == 0 { 1 } else { 2 };
        if pick.windows(2).any(|w| w[0] > w[1]) {
            c += 1;
        }
        best = Some(best.map_or(c, |b| b.min(c)));
        if best == Some(0) {
            break;
        }
    }
    best.map(|c| c + context_cost(query, decl) + anything_cost(query, decl))
}

/// A declaration whose result is a bare type variable -- `raise : a -> b`,
/// `const : a -> b -> a` -- produces whatever the query asks for, and so
/// answers every query anybody types. It is rarely what they meant, and
/// without a price the list would be full of them.
fn anything_cost(query: &Sig, decl: &Sig) -> u32 {
    let asked_for_something = !matches!(query.result, Ty::Var(_) | Ty::Any);
    if asked_for_something && matches!(decl.result, Ty::Var(_)) {
        3
    } else {
        0
    }
}

/// A trait the query names that the declaration does not ask for is a small
/// mismatch; one the declaration asks for that the query did not mention is a
/// smaller one, and nothing at all if the query names none -- most people do
/// not think about traits when they search.
fn context_cost(query: &Sig, decl: &Sig) -> u32 {
    if query.context.is_empty() {
        return 0;
    }
    let missing = query
        .context
        .iter()
        .filter(|c| !decl.context.contains(c))
        .count();
    let extra = decl
        .context
        .iter()
        .filter(|c| !query.context.contains(c))
        .count();
    (2 * missing + extra) as u32
}

/// Every way of giving `m` query arguments to `n` declaration arguments, one
/// each: the first `j` in each is where the query's first argument goes.
///
/// All of them while there are few; past that, only the ones that keep the
/// query's order, since trying every permutation of a long argument list is
/// the search taking longer than it is worth.
fn pairings(m: usize, n: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let every = permutations_count(n, m) <= 120;
    let mut current = Vec::with_capacity(m);
    let mut used = vec![false; n];
    fn go(
        m: usize,
        n: usize,
        every: bool,
        current: &mut Vec<usize>,
        used: &mut Vec<bool>,
        out: &mut Vec<Vec<usize>>,
    ) {
        if current.len() == m {
            out.push(current.clone());
            return;
        }
        let from = if every {
            0
        } else {
            current.last().map_or(0, |&j| j + 1)
        };
        for j in from..n {
            if used[j] {
                continue;
            }
            used[j] = true;
            current.push(j);
            go(m, n, every, current, used, out);
            current.pop();
            used[j] = false;
        }
    }
    go(m, n, every, &mut current, &mut used, &mut out);
    out
}

fn permutations_count(n: usize, m: usize) -> usize {
    (n - m + 1..=n).product::<usize>().max(1)
}

/// A declaration's type, its variables moved clear of the query's.
fn rename(t: &Ty) -> Ty {
    match t {
        Ty::Var(v) => Ty::Var(DECL + v),
        Ty::Con(n, args) => Ty::Con(n.clone(), args.iter().map(rename).collect()),
        Ty::Fun(args, r) => Ty::Fun(args.iter().map(rename).collect(), Box::new(rename(r))),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(rename).collect()),
        Ty::Record(fields) => {
            Ty::Record(fields.iter().map(|(l, t)| (l.clone(), rename(t))).collect())
        }
        Ty::Any => Ty::Any,
    }
}

#[derive(Clone, Default)]
struct Subst(HashMap<u32, Ty>);

impl Subst {
    fn walk(&self, t: &Ty) -> Ty {
        let mut t = t.clone();
        while let Ty::Var(v) = t {
            match self.0.get(&v) {
                Some(next) => t = next.clone(),
                None => break,
            }
        }
        t
    }

    fn resolve(&self, t: &Ty) -> Ty {
        match self.walk(t) {
            Ty::Con(n, args) => Ty::Con(n, args.iter().map(|a| self.resolve(a)).collect()),
            Ty::Fun(args, r) => Ty::Fun(
                args.iter().map(|a| self.resolve(a)).collect(),
                Box::new(self.resolve(&r)),
            ),
            Ty::Tuple(items) => Ty::Tuple(items.iter().map(|a| self.resolve(a)).collect()),
            Ty::Record(fields) => Ty::Record(
                fields
                    .into_iter()
                    .map(|(l, t)| (l, self.resolve(&t)))
                    .collect(),
            ),
            other => other,
        }
    }

    fn occurs(&self, v: u32, t: &Ty) -> bool {
        match self.walk(t) {
            Ty::Var(w) => w == v,
            Ty::Con(_, args) | Ty::Tuple(args) => args.iter().any(|a| self.occurs(v, a)),
            Ty::Fun(args, r) => args.iter().any(|a| self.occurs(v, a)) || self.occurs(v, &r),
            Ty::Record(fields) => fields.iter().any(|(_, t)| self.occurs(v, t)),
            Ty::Any => false,
        }
    }

    fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let (a, b) = (self.walk(a), self.walk(b));
        match (&a, &b) {
            (Ty::Any, _) | (_, Ty::Any) => true,
            (Ty::Var(x), Ty::Var(y)) if x == y => true,
            (Ty::Var(x), t) | (t, Ty::Var(x)) => {
                if self.occurs(*x, t) {
                    return false;
                }
                self.0.insert(*x, t.clone());
                true
            }
            (Ty::Con(n1, a1), Ty::Con(n2, a2)) => {
                n1 == n2 && a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| self.unify(x, y))
            }
            (Ty::Fun(p1, r1), Ty::Fun(p2, r2)) => {
                p1.len() == p2.len()
                    && p1.iter().zip(p2).all(|(x, y)| self.unify(x, y))
                    && self.unify(r1, r2)
            }
            (Ty::Tuple(a1), Ty::Tuple(a2)) => {
                a1.len() == a2.len() && a1.iter().zip(a2).all(|(x, y)| self.unify(x, y))
            }
            (Ty::Record(f1), Ty::Record(f2)) => {
                f1.len() == f2.len()
                    && f1
                        .iter()
                        .zip(f2)
                        .all(|((l1, x), (l2, y))| l1 == l2 && self.unify(x, y))
            }
            _ => false,
        }
    }

    /// What the variables had to become for the match to hold. See the module
    /// docs for why each costs what it does.
    fn price(&self, query: &Sig, decl: &Sig) -> u32 {
        let mut qvars = Vec::new();
        for t in query.args.iter().chain(std::iter::once(&query.result)) {
            vars(t, &mut qvars);
        }
        let mut dvars = Vec::new();
        for t in decl.args.iter().chain(std::iter::once(&decl.result)) {
            vars(&rename(t), &mut dvars);
        }
        qvars.sort_unstable();
        qvars.dedup();
        dvars.sort_unstable();
        dvars.dedup();

        let mut cost = 0;
        // Which variable each ended up as, for the ones that stayed variables.
        let mut classes: HashMap<u32, (u32, u32)> = HashMap::new();
        for &v in &qvars {
            match self.resolve(&Ty::Var(v)) {
                Ty::Var(root) => classes.entry(root).or_default().0 += 1,
                Ty::Any => {}
                _ => cost += 3,
            }
        }
        for &v in &dvars {
            match self.resolve(&Ty::Var(v)) {
                Ty::Var(root) => classes.entry(root).or_default().1 += 1,
                Ty::Any => {}
                _ => cost += 1,
            }
        }
        for (q, d) in classes.values() {
            // Two of the query's variables forced to be one: the declaration
            // is less general than asked.
            cost += 2 * q.saturating_sub(1);
            // Two of the declaration's made one by the query: more general
            // than asked, which is the cheaper way round.
            cost += d.saturating_sub(1);
        }
        cost
    }
}

fn vars(t: &Ty, out: &mut Vec<u32>) {
    match t {
        Ty::Var(v) => out.push(*v),
        Ty::Con(_, args) | Ty::Tuple(args) => args.iter().for_each(|a| vars(a, out)),
        Ty::Fun(args, r) => {
            args.iter().for_each(|a| vars(a, out));
            vars(r, out);
        }
        Ty::Record(fields) => fields.iter().for_each(|(_, t)| vars(t, out)),
        Ty::Any => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(s: &str) -> Sig {
        parse(s)
    }

    #[test]
    fn a_query_is_read_as_a_signature_is_written() {
        let s = sig("Show a => [a] -> (a -> b) -> #[b]");
        assert_eq!(s.context, vec!["Show"]);
        assert_eq!(
            s.args,
            vec![
                Ty::Con("Vector".into(), vec![Ty::Var(0)]),
                Ty::Fun(vec![Ty::Var(0)], Box::new(Ty::Var(1))),
            ]
        );
        assert_eq!(s.result, Ty::Con("Array".into(), vec![Ty::Var(1)]));
    }

    #[test]
    fn what_is_not_typed_yet_is_anything() {
        let s = sig("[a] -> ");
        assert_eq!(s.args.len(), 1);
        assert_eq!(s.result, Ty::Any);
        assert_eq!(sig("Maybe").result, Ty::Con("Maybe".into(), vec![]));
        assert_eq!(
            sig("(Int, ").result,
            Ty::Tuple(vec![Ty::Con("Int".into(), vec![]), Ty::Any])
        );
        assert_eq!(sig("()").result, Ty::Con("Unit".into(), vec![]));
        assert_eq!(
            sig("Std.Maybe.Maybe a").result,
            Ty::Con("Maybe".into(), vec![Ty::Var(0)])
        );
    }

    #[test]
    fn the_same_type_under_other_names_costs_nothing() {
        assert_eq!(cost(&sig("[a] -> Int"), &sig("[b] -> Int")), Some(0));
        assert_eq!(
            cost(
                &sig("(a -> b) -> [a] -> [b]"),
                &sig("(x -> y) -> [x] -> [y]")
            ),
            Some(0)
        );
    }

    #[test]
    fn a_different_result_is_no_match() {
        assert_eq!(cost(&sig("[a] -> Int"), &sig("[a] -> String")), None);
        assert_eq!(
            cost(&sig("a -> b -> c"), &sig("a -> c")),
            None,
            "more arguments than it takes"
        );
    }

    #[test]
    fn arguments_in_another_order_still_match() {
        let take = sig("Int -> [a] -> [a]");
        assert_eq!(cost(&sig("[a] -> Int -> [a]"), &take), Some(1));
        assert_eq!(cost(&sig("Int -> [a] -> [a]"), &take), Some(0));
    }

    #[test]
    fn more_general_is_better_than_less() {
        // Asked for `Int`, the declaration works for any `a`: fine.
        let general = cost(&sig("[Int] -> Int"), &sig("[a] -> Int")).unwrap();
        // Asked for any `a`, the declaration only works for `Int`: worse.
        let specific = cost(&sig("[a] -> Int"), &sig("[Int] -> Int")).unwrap();
        assert!(general < specific, "{general} < {specific}");
    }

    #[test]
    fn taking_more_than_was_asked_costs_something() {
        let foldl = sig("(b -> a -> b) -> b -> [a] -> b");
        let asked = cost(&sig("[a] -> b"), &foldl).unwrap();
        assert!(asked > 0);
        let exact = cost(&sig("(b -> a -> b) -> b -> [a] -> b"), &foldl).unwrap();
        assert!(exact < asked);
    }

    #[test]
    fn a_result_alone_finds_what_produces_it() {
        let q = sig("String");
        assert_eq!(cost(&q, &sig("String")), Some(0));
        assert!(cost(&q, &sig("Show a => a -> String")).is_some());
        assert!(cost(&q, &sig("Int -> Int")).is_none());
    }

    #[test]
    fn a_variable_cannot_be_two_different_types() {
        // `a -> a -> Bool` cannot be `Int -> String -> Bool`.
        assert_eq!(
            cost(&sig("Int -> String -> Bool"), &sig("a -> a -> Bool")),
            None
        );
    }

    #[test]
    fn what_produces_anything_comes_after_what_produces_the_thing_asked() {
        let q = sig("[a] -> Int");
        let length = cost(&q, &sig("[a] -> Int")).unwrap();
        let raise = cost(&q, &sig("a -> b")).unwrap();
        let general = cost(&q, &sig("a -> Int")).unwrap();
        assert!(
            length < general && general < raise,
            "{length} < {general} < {raise}"
        );
    }

    #[test]
    fn a_trait_the_query_names_is_looked_for() {
        let eq = sig("Eq a => a -> [a] -> Bool");
        let plain = sig("a -> [a] -> Bool");
        let q = sig("Eq a => a -> [a] -> Bool");
        assert!(cost(&q, &eq).unwrap() < cost(&q, &plain).unwrap());
        // Unmentioned, it makes no difference.
        let q = sig("a -> [a] -> Bool");
        assert_eq!(cost(&q, &eq), cost(&q, &plain));
    }
}

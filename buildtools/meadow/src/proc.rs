//! Running a procedural macro while the package that calls it is compiled.
//!
//! The compiler cannot do this on its own: a macro is a compiled function, and
//! running one means linking a program and evaluating it, which is this crate's
//! job. So the compiler asks, through
//! [`Runner`](meadow_compiler::expand::proc::Runner), and this answers.
//!
//! What crosses is `Std.Macro`'s `TokenTree`, and the `Datum`s of the
//! compile-time bindings the call can see. Going in, both are built as terms
//! -- `Array`s of constructors, handed to `Vector.fromArray`, which is how a
//! `[a]` is made without knowing how one is laid out. The macro is run under
//! `Std.Macro.expanding`, the handler of its `Expand` effect, which answers
//! its `lookup`s from those bindings and writes what happened into a `Wire`:
//! arrays all the way down, which the evaluator hands over as plain `Vec`s,
//! so nothing here has to know how a `[a]` is laid out either.
//!
//! Two things make this safe to do during a build. A macro's signature forbids
//! the effects that would let it learn anything about the world, so what it
//! answers depends only on what it was given and what it read -- which is why
//! the answers are cached here on exactly that. And it runs with a step
//! budget, so a macro that does not stop fails the build instead of hanging
//! it.

use meadow_compiler::expand::Datum;
use meadow_compiler::expand::proc::{Failure, Outcome, Runner, Scope};
use meadow_compiler::span::Span;
use meadow_compiler::{CompiledPackage, core, intern::InternedString, lexer::tt};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// How many steps a macro may take before the build gives up on it.
///
/// Generous -- a macro that builds a table from a hundred patterns does real
/// work -- but not unbounded: a macro that does not stop has to fail the build
/// rather than hang it, and a minute of silence is already too long to be a
/// good error. `MEADOW_MACRO_FUEL` raises or lowers it for the rare macro that
/// needs more, and for the tests that check what happens when one does not
/// stop.
const FUEL: u64 = 50_000_000;

fn fuel() -> u64 {
    std::env::var("MEADOW_MACRO_FUEL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(FUEL)
}

/// A call, by the macro, its argument as written, and whether the round it
/// ran in was settled.
type AnswerKey = (InternedString, InternedString, String, bool);

/// The bindings a run read, and what each said -- `None` for one that was not
/// there.
type Reads = Vec<(String, Option<Datum>)>;

/// Where the packages came from.
///
/// A build has them to hand and lends them for as long as the compile takes; a
/// language server holds a runner for as long as a package is open, which is
/// longer than anything it could borrow from. Cloning is why this is a choice
/// and not a rule: a build compiles many packages and would pay for it every
/// time.
enum Held<'a> {
    Borrowed(Vec<&'a CompiledPackage>),
    Owned(Vec<CompiledPackage>),
}

impl Held<'_> {
    fn packages(&self) -> Vec<&CompiledPackage> {
        match self {
            Held::Borrowed(ps) => ps.clone(),
            Held::Owned(ps) => ps.iter().collect(),
        }
    }
}

/// The macros a build can run: every package compiled so far, linked on demand.
///
/// Linked only when a macro is actually called -- which in most builds is
/// never, and linking is not free.
pub struct Macros<'a> {
    /// The packages a macro could live in, and what it needs to run.
    packages: Held<'a>,
    /// How many steps any one of them may take.
    fuel: u64,
    /// The linked program, built the first time a macro is actually called --
    /// most builds call none, and linking is not free.
    program: RefCell<Option<Arc<core::Program>>>,
    /// What each call answered, by what it was asked and what it read. A
    /// macro cannot tell one build from another, so the same tokens and the
    /// same bindings always give the same answer.
    answers: RefCell<HashMap<AnswerKey, Vec<(Reads, Outcome)>>>,
}

impl<'a> Macros<'a> {
    pub fn new(packages: Vec<&'a CompiledPackage>) -> Macros<'a> {
        Macros {
            packages: Held::Borrowed(packages),
            fuel: fuel(),
            program: RefCell::new(None),
            answers: RefCell::new(HashMap::new()),
        }
    }

    /// The same, holding the packages itself: what something that outlives the
    /// compile -- a language server -- needs.
    pub fn owning(packages: Vec<CompiledPackage>) -> Macros<'static> {
        Macros {
            packages: Held::Owned(packages),
            fuel: fuel(),
            program: RefCell::new(None),
            answers: RefCell::new(HashMap::new()),
        }
    }

    /// The same, with a budget of its own: what a test that wants to see a
    /// macro run out of one uses, so it can do it in a moment.
    pub fn with_fuel(packages: Vec<&'a CompiledPackage>, fuel: u64) -> Macros<'a> {
        Macros {
            fuel,
            ..Macros::new(packages)
        }
    }

    /// The linked program, linked once.
    fn program(&self) -> Arc<core::Program> {
        if let Some(p) = self.program.borrow().as_ref() {
            return p.clone();
        }
        let linked =
            crate::linker::Linker::link(self.packages.packages().into_iter().cloned().collect());
        let p = Arc::new(linked.program);
        *self.program.borrow_mut() = Some(p.clone());
        p
    }

    /// The variable a package exports under `name`.
    fn exported(&self, package: InternedString, name: InternedString) -> Option<core::Var> {
        self.packages
            .packages()
            .into_iter()
            .filter(|p| p.name == package || p.ident == package)
            .flat_map(|p| p.exports.iter())
            .find(|e| e.name == name)
            .map(|e| e.var)
    }

    /// A function of `Std`, by the module it is in and its name.
    fn std_fn(&self, module: &[&str], name: &str) -> Option<core::Var> {
        let want: Vec<InternedString> = module.iter().map(|m| InternedString::from(*m)).collect();
        let name = InternedString::from(name);
        self.packages
            .packages()
            .into_iter()
            .flat_map(|p| p.exports.iter())
            .find(|e| e.name == name && e.module == want)
            .map(|e| e.var)
    }

    /// The canonical name of a constructor spelled `Type.Ctor`, which carries
    /// the package that declared it and so cannot be written out here.
    ///
    /// The standard library's are looked in first: every one this asks for is
    /// its, and a package with a `Datum` of its own must not be taken for it.
    fn ctor(&self, spelled: &str) -> Option<InternedString> {
        let mut packages = self.packages.packages();
        packages.sort_by_key(|p| p.name.to_string() != "Std");
        packages
            .into_iter()
            .flat_map(|p| p.variants.values())
            .flat_map(|vs| vs.iter())
            .map(|v| v.name)
            .find(|n| meadow_compiler::hir::spelling(&n.to_string()) == spelled)
    }
}

impl Runner for Macros<'_> {
    fn run(
        &self,
        package: InternedString,
        name: InternedString,
        input: &[tt::TokenTree],
        at: Span,
        scope: &Scope<'_>,
    ) -> Result<Outcome, Failure> {
        let key = (package, name, tt::render(input), scope.settled);
        // An answer is good for as long as everything it read still says what
        // it said: the argument is the key, and the reads are checked here.
        if let Some(had) = self.answers.borrow().get(&key) {
            let current = |n: &str| {
                scope
                    .visible
                    .iter()
                    .rev()
                    .find(|(k, _)| k == n)
                    .map(|(_, d)| d)
            };
            if let Some((_, out)) = had
                .iter()
                .find(|(reads, _)| reads.iter().all(|(n, was)| current(n) == was.as_ref()))
            {
                return Ok(out.clone());
            }
        }
        let f = self
            .exported(package, name)
            .ok_or_else(|| format!("`{package}` does not export `{name}`"))?;
        let from_array = self
            .std_fn(&["Collections", "Vector"], "fromArray")
            .ok_or("the standard library has no `Vector.fromArray`")?;
        let expanding = self
            .std_fn(&["Macro"], "expanding")
            .ok_or("the standard library has no `Macro.expanding`")?;
        let mut enc = Encode {
            from_array,
            ctor: &|bare| self.ctor(bare),
            spans: true,
        };
        let argument = enc.trees(input)?;
        // Later bindings shadow earlier ones, and the handler takes the first
        // it finds: so the list goes in latest first.
        let bound: Vec<core::Term> = scope
            .visible
            .iter()
            .rev()
            .map(|(n, d)| Ok(core::Term::Tuple(vec![text(n), enc.datum(d)?])))
            .collect::<Result<_, Failure>>()?;
        let bound = core::Term::App(
            Arc::new(core::Term::Var(from_array)),
            Arc::new(core::Term::Array(bound, unknown())),
        );
        // `expanding settled scope macro argument`: the macro run under the
        // handler of its `Expand` effect, answering in a `Wire` -- arrays all
        // the way down, which is what can be read here without knowing how a
        // `[a]` is laid out.
        let call = [
            core::Term::Lit(core::Lit::Bool(scope.settled)),
            bound,
            core::Term::Var(f),
            argument,
        ]
        .into_iter()
        .fold(core::Term::Var(expanding), |g, x| {
            core::Term::App(Arc::new(g), Arc::new(x))
        });
        let value = meadow_eval::eval_with_fuel(&self.program(), Arc::new(call), self.fuel)
            .map_err(|e| Failure::from(e.msg))?;
        let out = outcome(&wire(&value)?, at)?;
        if let Outcome::Answered { read, .. } = &out {
            let current = |n: &str| {
                scope
                    .visible
                    .iter()
                    .rev()
                    .find(|(k, _)| k == n)
                    .map(|(_, d)| d.clone())
            };
            let reads = read.iter().map(|n| (n.clone(), current(n))).collect();
            self.answers
                .borrow_mut()
                .entry(key)
                .or_default()
                .push((reads, out.clone()));
        }
        Ok(out)
    }
}

/// Building the argument: token trees as a term the evaluator can run.
struct Encode<'a> {
    from_array: core::Var,
    ctor: &'a dyn Fn(&str) -> Option<InternedString>,
    /// Whether tokens go in where they were written. A binding's code does
    /// not: it was written in whatever file defined it, and where it is spliced
    /// is somewhere else, so it stands `Nowhere` -- at the call that uses it.
    spans: bool,
}

impl Encode<'_> {
    /// `fromArray #[…]` -- the only way to build a `[a]` without knowing how
    /// one is laid out.
    fn trees(&mut self, trees: &[tt::TokenTree]) -> Result<core::Term, String> {
        let items: Result<Vec<core::Term>, String> = trees.iter().map(|t| self.tree(t)).collect();
        Ok(core::Term::App(
            Arc::new(core::Term::Var(self.from_array)),
            Arc::new(core::Term::Array(items?, unknown())),
        ))
    }

    fn tree(&mut self, tree: &tt::TokenTree) -> Result<core::Term, String> {
        use meadow_compiler::lexer::Token;
        let (ctor, mut fields) = match tree {
            tt::TokenTree::Group(g) => {
                let delim = self.delim(g.delim)?;
                let inner = self.trees(&g.trees)?;
                ("Group", vec![delim, inner])
            }
            tt::TokenTree::Token(t) => match t.value() {
                Token::LowerIdent(s) | Token::UpperIdent(s) => ("Word", vec![text(&s.to_string())]),
                Token::String(s) => ("Str", vec![text(&s.to_string())]),
                Token::Char(c) => ("Chr", vec![text(&c.to_string())]),
                Token::Int(n) => ("Num", vec![core::Term::Lit(core::Lit::Int(*n))]),
                Token::Real(bits) => (
                    "Real",
                    vec![core::Term::Lit(core::Lit::Float(f64::from_bits(*bits)))],
                ),
                // A keyword is a word: `data` and `fun` are spelled like any
                // other name, and a macro reading a declaration wants them
                // that way. Everything else is punctuation, by the text it is
                // written with.
                other => {
                    let written = other.text();
                    let word = written.starts_with(|c: char| c.is_alphabetic());
                    (if word { "Word" } else { "Punct" }, vec![text(&written)])
                }
            },
        };
        let span = match tree {
            tt::TokenTree::Group(g) => Span::new(g.open.start, g.close.end),
            tt::TokenTree::Token(t) => t.span,
        };
        fields.push(self.loc(span)?);
        self.build(&format!("TokenTree.{ctor}"), fields)
    }

    /// Where a token was written, as a `Std.Macro.Loc`.
    fn loc(&mut self, span: Span) -> Result<core::Term, String> {
        if !self.spans {
            return self.build("Loc.Nowhere", Vec::new());
        }
        let at = |n: u32| core::Term::Lit(core::Lit::Int(n as i64));
        self.build("Loc.At", vec![at(span.start), at(span.end)])
    }

    fn delim(&mut self, d: tt::Delim) -> Result<core::Term, String> {
        let name = match d {
            tt::Delim::Paren => "Paren",
            tt::Delim::Brack => "Bracket",
            tt::Delim::Brace => "Brace",
        };
        self.build(&format!("Delim.{name}"), Vec::new())
    }

    /// A compile-time binding's value, as the `Std.Macro.Datum` it is.
    fn datum(&mut self, d: &Datum) -> Result<core::Term, String> {
        let (ctor, fields) = match d {
            Datum::Sym(s) => ("Sym", vec![text(s)]),
            Datum::Str(s) => ("Str", vec![text(s)]),
            Datum::Int(n) => ("Int", vec![core::Term::Lit(core::Lit::Int(*n))]),
            Datum::Float(f) => ("Float", vec![core::Term::Lit(core::Lit::Float(*f))]),
            Datum::Bool(b) => ("Bool", vec![core::Term::Lit(core::Lit::Bool(*b))]),
            Datum::List(ds) => ("List", vec![self.vector(ds)?]),
            Datum::Rec(fs) => {
                let fields: Vec<core::Term> = fs
                    .iter()
                    .map(|(k, v)| Ok(core::Term::Tuple(vec![text(k), self.datum(v)?])))
                    .collect::<Result<_, String>>()?;
                ("Rec", vec![self.array(fields)])
            }
            Datum::Tag(t, ds) => ("Tag", vec![text(t), self.vector(ds)?]),
            Datum::Code(ts) => {
                let was = std::mem::replace(&mut self.spans, false);
                let trees = self.trees(ts);
                self.spans = was;
                ("Code", vec![trees?])
            }
        };
        self.build(&format!("Datum.{ctor}"), fields)
    }

    /// `[…]` of datums.
    fn vector(&mut self, ds: &[Datum]) -> Result<core::Term, String> {
        let items = ds
            .iter()
            .map(|d| self.datum(d))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.array(items))
    }

    /// `fromArray #[…]`.
    fn array(&self, items: Vec<core::Term>) -> core::Term {
        core::Term::App(
            Arc::new(core::Term::Var(self.from_array)),
            Arc::new(core::Term::Array(items, unknown())),
        )
    }

    fn build(&mut self, spelled: &str, fields: Vec<core::Term>) -> Result<core::Term, String> {
        let name = (self.ctor)(spelled)
            .ok_or_else(|| format!("the standard library has no `{spelled}`"))?;
        Ok(core::Term::Ctor(name, unknown(), fields))
    }
}

fn text(s: &str) -> core::Term {
    core::Term::Lit(core::Lit::Str(InternedString::from(s)))
}

/// The type a constructor builds. Nothing reads it: this term is evaluated, not
/// checked -- it was built here rather than written by anyone.
fn unknown() -> core::Ty {
    meadow_compiler::infer::Type::Error
}

/// `Std.Macro.Wire`: what a run answers with, read out of the evaluator's
/// values. Arrays all the way down, so nothing here has to know how a `[a]`
/// is laid out.
enum Wire {
    Node(String, Vec<Wire>),
    Text(String),
    Whole(i64),
    Frac(f64),
}

fn wire(value: &meadow_eval::Value) -> Result<Wire, String> {
    let bad = || "it did not answer with tokens".to_string();
    let meadow_eval::Value::Ctor(name, fields) = value else {
        return Err(bad());
    };
    let spelled = meadow_compiler::hir::spelling(&name.to_string()).to_string();
    let field = |i: usize| fields.get(i).ok_or_else(bad);
    match spelled.rsplit('.').next().unwrap_or_default() {
        "Node" => {
            let meadow_eval::Value::Str(tag) = field(0)? else {
                return Err(bad());
            };
            let meadow_eval::Value::Array(items) = field(1)? else {
                return Err(bad());
            };
            Ok(Wire::Node(
                tag.to_string(),
                items.iter().map(wire).collect::<Result<_, _>>()?,
            ))
        }
        "Text" => match field(0)? {
            meadow_eval::Value::Str(s) => Ok(Wire::Text(s.to_string())),
            _ => Err(bad()),
        },
        "Whole" => match field(0)? {
            meadow_eval::Value::Int(n) => Ok(Wire::Whole(*n)),
            _ => Err(bad()),
        },
        "Frac" => match field(0)? {
            meadow_eval::Value::Float(f) => Ok(Wire::Frac(*f)),
            _ => Err(bad()),
        },
        _ => Err(bad()),
    }
}

/// The children of a node, whatever it is called.
fn children(w: &Wire) -> Result<&[Wire], String> {
    match w {
        Wire::Node(_, xs) => Ok(xs),
        _ => Err("a list was expected".to_string()),
    }
}

fn wire_text(w: &Wire) -> Result<String, String> {
    match w {
        Wire::Text(s) => Ok(s.clone()),
        _ => Err("text was expected".to_string()),
    }
}

/// How the run ended.
fn outcome(w: &Wire, at: Span) -> Result<Outcome, Failure> {
    match w {
        Wire::Node(tag, xs) if tag == "Answered" && xs.len() == 3 => {
            let trees = decode_trees(&xs[0], at)?;
            let defined = children(&xs[1])?
                .iter()
                .map(|d| match d {
                    Wire::Node(name, v) if v.len() == 1 => Ok((name.clone(), datum(&v[0], at)?)),
                    _ => Err(Failure::from("a definition without a value")),
                })
                .collect::<Result<_, Failure>>()?;
            let read = children(&xs[2])?
                .iter()
                .map(wire_text)
                .collect::<Result<_, String>>()?;
            Ok(Outcome::Answered {
                trees,
                defined,
                read,
            })
        }
        Wire::Node(tag, xs) if tag == "Waiting" && xs.len() == 1 => {
            Ok(Outcome::Waiting(wire_text(&xs[0])?))
        }
        _ => Err(Failure::from("it did not answer with tokens")),
    }
}

/// A `Datum`, read back.
fn datum(w: &Wire, at: Span) -> Result<Datum, Failure> {
    let Wire::Node(tag, xs) = w else {
        return Err(Failure::from("a value that is not a `Datum`"));
    };
    let one = || {
        xs.first()
            .ok_or_else(|| format!("`{tag}` with nothing in it"))
    };
    Ok(match tag.as_str() {
        "Sym" => Datum::Sym(wire_text(one()?)?),
        "Str" => Datum::Str(wire_text(one()?)?),
        "Int" => match one()? {
            Wire::Whole(n) => Datum::Int(*n),
            _ => return Err(Failure::from("`Int` of something that is not a number")),
        },
        "Float" => match one()? {
            Wire::Frac(f) => Datum::Float(*f),
            _ => return Err(Failure::from("`Float` of something that is not a number")),
        },
        "Bool" => Datum::Bool(matches!(one()?, Wire::Whole(n) if *n != 0)),
        "List" => Datum::List(xs.iter().map(|x| datum(x, at)).collect::<Result<_, _>>()?),
        "Rec" => Datum::Rec(
            xs.iter()
                .map(|f| match f {
                    Wire::Node(k, v) if v.len() == 1 => Ok((k.clone(), datum(&v[0], at)?)),
                    _ => Err(Failure::from("a field without a value")),
                })
                .collect::<Result<_, Failure>>()?,
        ),
        "Tag" if xs.len() == 2 => Datum::Tag(
            wire_text(&xs[0])?,
            children(&xs[1])?
                .iter()
                .map(|x| datum(x, at))
                .collect::<Result<_, _>>()?,
        ),
        "Code" => Datum::Code(decode_trees(one()?, at)?),
        other => return Err(Failure::from(format!("`{other}` is not a `Datum`"))),
    })
}

/// Token trees, read back. `Code` is text, which is lexed here -- that is the
/// point of it -- so one tree can be several.
fn decode_trees(w: &Wire, at: Span) -> Result<Vec<tt::TokenTree>, Failure> {
    let mut out = Vec::new();
    for t in children(w)? {
        out.extend(decode(t, at)?);
    }
    Ok(out)
}

fn decode(w: &Wire, at: Span) -> Result<Vec<tt::TokenTree>, Failure> {
    use meadow_compiler::lexer::{LToken, Token};
    let Wire::Node(tag, xs) = w else {
        return Err(Failure::from("it did not answer with tokens"));
    };
    let first = || {
        xs.first()
            .ok_or_else(|| format!("`{tag}` with nothing in it"))
    };
    // Where the token stands: where it was written, or -- for one the macro
    // made up -- where the call is.
    let placed = |i: usize| xs.get(i).and_then(written).unwrap_or(at);
    let one = |t: Token, i: usize| Ok(vec![tt::TokenTree::Token(LToken::new(t, placed(i)))]);
    match tag.as_str() {
        // A word is whatever the lexer makes of it: a keyword is a keyword,
        // and an identifier's case says which kind it is.
        "Word" | "Punct" => Ok(lex(&wire_text(first()?)?, placed(1))?),
        "Code" => Ok(lex(&wire_text(first()?)?, at)?),
        "Str" => one(Token::String(InternedString::from(wire_text(first()?)?)), 1),
        "Chr" => {
            let s = wire_text(first()?)?;
            let mut cs = s.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => one(Token::Char(c), 1),
                _ => Err(Failure::from("a character has to be one character")),
            }
        }
        "Num" => match first()? {
            Wire::Whole(n) => one(Token::Int(*n), 1),
            _ => Err(Failure::from(
                "`Num` was given something that is not a number",
            )),
        },
        "Real" => match first()? {
            Wire::Frac(f) => one(Token::Real(f.to_bits()), 1),
            _ => Err(Failure::from(
                "`Real` was given something that is not a number",
            )),
        },
        // Not tokens: what the macro has to say about what it was given, and
        // where -- the token that was wrong, when it said one.
        "Fail" => Err(Failure {
            msg: wire_text(first()?)?,
            at: xs.get(1).and_then(written),
        }),
        "Group" if xs.len() >= 2 => {
            let delim = match wire_text(&xs[0])?.as_str() {
                "(" => tt::Delim::Paren,
                "[" => tt::Delim::Brack,
                "{" => tt::Delim::Brace,
                _ => return Err(Failure::from("a group with no bracket")),
            };
            let span = placed(2);
            Ok(vec![tt::TokenTree::Group(tt::Group {
                delim,
                trees: decode_trees(&xs[1], at)?,
                open: Span::new(span.start, (span.start + 1).min(span.end)),
                close: Span::new(span.end.saturating_sub(1).max(span.start), span.end),
            })])
        }
        other => Err(Failure::from(format!("`{other}` is not a token"))),
    }
}

/// A `Loc`, read back: `Some` of where a token was written, `None` for one
/// written `Nowhere`.
fn written(w: &Wire) -> Option<Span> {
    match w {
        Wire::Node(tag, xs) if tag == "At" => match (xs.first(), xs.get(1)) {
            (Some(Wire::Whole(a)), Some(Wire::Whole(b))) if *a >= 0 && *b >= *a => {
                Some(Span::new(*a as u32, *b as u32))
            }
            _ => None,
        },
        _ => None,
    }
}

/// `text` as tokens, lexed the way a file is.
fn lex(text: &str, at: Span) -> Result<Vec<tt::TokenTree>, String> {
    use meadow_compiler::source::{Source, SourceKind};
    let src = Source::new(SourceKind::Interactive, InternedString::from(text));
    let lexed = meadow_compiler::lexer::tokenize(src);
    if let Some(e) = lexed.errors.first() {
        return Err(format!("`{text}` is not Meadow: {}", e.msg));
    }
    // Every token stands where the call is: it was not written anywhere else.
    let moved: Vec<meadow_compiler::lexer::LToken> = lexed
        .tokens
        .iter()
        .map(|t| meadow_compiler::lexer::LToken::new(t.value().clone(), at))
        .collect();
    let (trees, errs) = tt::trees(&moved, "a macro");
    if let Some(e) = errs.first() {
        return Err(format!("`{text}` does not balance: {}", e.msg));
    }
    Ok(trees)
}

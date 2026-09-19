//! Running a procedural macro while the package that calls it is compiled.
//!
//! The compiler cannot do this on its own: a macro is a compiled function, and
//! running one means linking a program and evaluating it, which is this crate's
//! job. So the compiler asks, through
//! [`Runner`](meadow_compiler::expand::proc::Runner), and this answers.
//!
//! What crosses is `Std.Macro`'s `TokenTree`. Going in, the call's argument is
//! built as a term -- an `Array` of constructors, handed to `Vector.fromArray`,
//! which is how a `[a]` is made without knowing how one is laid out. Coming
//! back, `Vector.toArray` turns the answer into an array the evaluator hands
//! over as a plain `Vec`, so nothing here has to know either.
//!
//! Two things make this safe to do during a build. A macro's signature forbids
//! the effects that would let it learn anything about the world, so what it
//! answers depends only on what it was given -- which is why the answers are
//! cached here on exactly that. And it runs with a step budget, so a macro
//! that does not stop fails the build instead of hanging it.

use meadow_compiler::expand::proc::Runner;
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
    /// What each call answered, by what it was asked. A macro cannot tell one
    /// build from another, so the same tokens always give the same tokens.
    answers: RefCell<HashMap<(InternedString, InternedString, String), Vec<tt::TokenTree>>>,
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
    fn ctor(&self, spelled: &str) -> Option<InternedString> {
        self.packages
            .packages()
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
    ) -> Result<Vec<tt::TokenTree>, String> {
        let key = (package, name, tt::render(input));
        if let Some(had) = self.answers.borrow().get(&key) {
            return Ok(had.clone());
        }
        let f = self
            .exported(package, name)
            .ok_or_else(|| format!("`{package}` does not export `{name}`"))?;
        let from_array = self
            .std_fn(&["Collections", "Vector"], "fromArray")
            .ok_or("the standard library has no `Vector.fromArray`")?;
        let to_array = self
            .std_fn(&["Collections", "Vector"], "toArray")
            .ok_or("the standard library has no `Vector.toArray`")?;
        let mut enc = Encode {
            from_array,
            ctor: &|bare| self.ctor(bare),
        };
        let argument = enc.trees(input)?;
        // `toArray (macro (fromArray #[…]))`: the answer comes back as an
        // array, which is a `Vec` here, so nothing has to know how a `[a]` is
        // laid out.
        let call = core::Term::App(
            Arc::new(core::Term::Var(to_array)),
            Arc::new(core::Term::App(
                Arc::new(core::Term::Var(f)),
                Arc::new(argument),
            )),
        );
        let value = meadow_eval::eval_with_fuel(&self.program(), Arc::new(call), self.fuel)
            .map_err(|e| e.msg)?;
        let out = decode_trees(&value, at)?;
        self.answers.borrow_mut().insert(key, out.clone());
        Ok(out)
    }
}

/// Building the argument: token trees as a term the evaluator can run.
struct Encode<'a> {
    from_array: core::Var,
    ctor: &'a dyn Fn(&str) -> Option<InternedString>,
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
        let (ctor, fields) = match tree {
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
        self.build(&format!("TokenTree.{ctor}"), fields)
    }

    fn delim(&mut self, d: tt::Delim) -> Result<core::Term, String> {
        let name = match d {
            tt::Delim::Paren => "Paren",
            tt::Delim::Brack => "Bracket",
            tt::Delim::Brace => "Brace",
        };
        self.build(&format!("Delim.{name}"), Vec::new())
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

/// Reading the answer back.
fn decode_trees(value: &meadow_eval::Value, at: Span) -> Result<Vec<tt::TokenTree>, String> {
    let meadow_eval::Value::Array(items) = value else {
        return Err("it did not answer with tokens".to_string());
    };
    items
        .iter()
        .map(|v| decode(v, at))
        .collect::<Result<Vec<_>, _>>()
        .map(|trees: Vec<Vec<tt::TokenTree>>| trees.into_iter().flatten().collect())
}

/// One `TokenTree` value as trees. `Code` is text, which is lexed here -- that
/// is the point of it -- so one value can be several trees.
fn decode(value: &meadow_eval::Value, at: Span) -> Result<Vec<tt::TokenTree>, String> {
    use meadow_compiler::lexer::{LToken, Token};
    let meadow_eval::Value::Ctor(name, fields) = value else {
        return Err("it did not answer with tokens".to_string());
    };
    let bare = meadow_compiler::hir::spelling(&name.to_string())
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_string();
    let one = |t: Token| Ok(vec![tt::TokenTree::Token(LToken::new(t, at))]);
    let string = |i: usize| -> Result<InternedString, String> {
        match fields.get(i) {
            Some(meadow_eval::Value::Str(s)) => Ok(*s),
            _ => Err(format!("`{bare}` was given something that is not text")),
        }
    };
    match bare.as_str() {
        "Word" => {
            let s = string(0)?;
            // A word is whatever the lexer makes of it: a keyword is a keyword,
            // and an identifier's case says which kind it is.
            lex(&s.to_string(), at)
        }
        "Punct" => lex(&string(0)?.to_string(), at),
        "Str" => one(Token::String(string(0)?)),
        "Chr" => {
            let s = string(0)?.to_string();
            let mut cs = s.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => one(Token::Char(c)),
                _ => Err("a character has to be one character".to_string()),
            }
        }
        "Num" => match fields.first() {
            Some(meadow_eval::Value::Int(n)) => one(Token::Int(*n)),
            _ => Err("`Num` was given something that is not a number".to_string()),
        },
        "Real" => match fields.first() {
            Some(meadow_eval::Value::Float(f)) => one(Token::Real(f.to_bits())),
            _ => Err("`Real` was given something that is not a number".to_string()),
        },
        "Code" => lex(&string(0)?.to_string(), at),
        // Not tokens: what the macro has to say about what it was given.
        "Fail" => Err(string(0)?.to_string()),
        "Group" => {
            let delim = match fields.first() {
                Some(meadow_eval::Value::Ctor(d, _)) => {
                    match meadow_compiler::hir::spelling(&d.to_string())
                        .rsplit('.')
                        .next()
                    {
                        Some("Paren") => tt::Delim::Paren,
                        Some("Bracket") => tt::Delim::Brack,
                        Some("Brace") => tt::Delim::Brace,
                        _ => return Err("a group with no bracket".to_string()),
                    }
                }
                _ => return Err("a group with no bracket".to_string()),
            };
            let inside = fields
                .get(1)
                .ok_or_else(|| "a group with nothing in it".to_string())?;
            Ok(vec![tt::TokenTree::Group(tt::Group {
                delim,
                trees: decode_trees(inside, at)?,
                open: Span::new(at.start, at.start),
                close: Span::new(at.end, at.end),
            })])
        }
        other => Err(format!("`{other}` is not a token")),
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

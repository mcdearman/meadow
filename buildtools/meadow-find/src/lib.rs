//! **Finding a declaration, by name or by type.**
//!
//! What the REPL's `Ctrl-F` picker and the language server's workspace symbols
//! both search: every declaration a program can reach -- values, functions,
//! constructors, types, traits, effects -- each with its signature, its doc
//! comment, and where it was written.
//!
//! A query is read one of two ways, and [`Query::parse`] decides which:
//!
//! - **By name**, fuzzily, the way Helix's pickers match -- the same matcher,
//!   `nucleo`. `len` finds `length`; `vec len` or `Vector.len` finds it in
//!   `Std.Collections.Vector` and not elsewhere.
//! - **By type**, the way Hoogle does, when the query is a type rather than a
//!   name: it has an arrow in it, or starts with `:`. `[a] -> Int` finds
//!   `length`; `: String` finds what makes one. See [`shape`] for what counts
//!   as a match and in what order.
//!
//! The index is a plain list of [`Decl`]s that can be written out and read back,
//! with nothing in it that needs the compiler that made it. That is so a
//! package registry can publish one for what it holds, and a search can take in
//! packages nobody has installed; each entry says which package it is from.

pub mod collect;
pub mod shape;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use serde::{Deserialize, Serialize};

/// One thing a program can name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decl {
    /// As a program writes it: `length`, `(++)`, `Maybe`, `Just`.
    pub name: String,
    /// The module it is declared in, by its full dotted path.
    pub module: String,
    /// The package it comes from.
    pub package: String,
    pub kind: Kind,
    /// For a value, its type -- `[a] -> Int` -- as a signature writes it. For a
    /// type, trait or effect, its declaration, on one line.
    pub detail: String,
    /// The value's type as a search compares it: see [`shape`]. `None` for
    /// what has no type of its own, a `data` declaration say.
    pub shape: Option<shape::Sig>,
    /// The `--` comment above it, if it has one.
    pub doc: Option<String>,
    /// Whether a program can name it with no `use` at all: its package's
    /// prelude exports it. Among answers equally good otherwise, these come
    /// first -- the `map` somebody means is the one they already have.
    #[serde(default)]
    pub prelude: bool,
    pub location: Option<Location>,
    /// Where it was written, in a form an editor can go to. Belongs to the
    /// process that compiled it, so it is not written out with the rest.
    #[serde(skip)]
    pub site: Option<Site>,
    /// Which binding it is, for asking whether a name in scope means this one
    /// or something else spelled the same. A value's only.
    #[serde(skip)]
    pub var: Option<meadow_compiler::hir::VarId>,
}

impl Decl {
    /// `Std.Collections.Vector.length`.
    pub fn qualified(&self) -> String {
        if self.module.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.module, self.name)
        }
    }

    /// One line saying what it is: `length : [a] -> Int` for a value, the
    /// declaration itself for a type.
    pub fn headline(&self) -> String {
        if self.kind.is_value() {
            format!("{} : {}", self.name, self.detail)
        } else {
            self.detail.clone()
        }
    }
}

/// Where a declaration was written, for a person to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// The file, as the compiler knew it: a path for a package's own modules,
    /// a name within the library for `Std`.
    pub file: String,
    /// One-based, as an editor counts.
    pub line: u32,
    pub column: u32,
}

/// Where a declaration was written, for a program to go to.
#[derive(Debug, Clone, Copy)]
pub struct Site {
    pub source: meadow_compiler::source::Source,
    pub span: meadow_compiler::span::Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Kind {
    Function,
    Value,
    /// A function written `@macro`.
    Macro,
    /// A trait's method.
    Method,
    /// An effect's operation.
    Operation,
    Constructor,
    Type,
    Record,
    Alias,
    Trait,
    Effect,
}

impl Kind {
    /// Something with a type of its own, that a program can use as a value.
    pub fn is_value(self) -> bool {
        matches!(
            self,
            Kind::Function
                | Kind::Value
                | Kind::Macro
                | Kind::Method
                | Kind::Operation
                | Kind::Constructor
        )
    }

    /// A word for it, short enough for a column.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Function => "fun",
            Kind::Value => "def",
            Kind::Macro => "macro",
            Kind::Method => "method",
            Kind::Operation => "op",
            Kind::Constructor => "ctor",
            Kind::Type => "data",
            Kind::Record => "record",
            Kind::Alias => "type",
            Kind::Trait => "trait",
            Kind::Effect => "effect",
        }
    }
}

/// What somebody typed, read as a name or as a type.
#[derive(Debug, Clone)]
pub enum Query {
    /// Nothing typed yet: everything, in order.
    Empty,
    Name(String),
    Type(shape::Sig),
}

impl Query {
    /// A type when it looks like one -- an arrow, a `=>`, or a leading `:` to
    /// say so outright -- and a name otherwise.
    ///
    /// A bare `Maybe` is a name: that is how most people would look for the
    /// type. `: Maybe a` asks for what makes one.
    pub fn parse(text: &str) -> Query {
        let t = text.trim();
        if t.is_empty() {
            return Query::Empty;
        }
        if let Some(rest) = t.strip_prefix(':') {
            return if rest.trim().is_empty() {
                Query::Empty
            } else {
                Query::Type(shape::parse(rest))
            };
        }
        if t.contains("->") || t.contains("=>") {
            return Query::Type(shape::parse(t));
        }
        Query::Name(t.to_string())
    }

    pub fn is_type(&self) -> bool {
        matches!(self, Query::Type(_))
    }
}

/// One answer: which declaration, and how good an answer it is -- higher is
/// better, whichever way the query was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub decl: usize,
    pub score: i64,
}

/// Every declaration a search can find.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub decls: Vec<Decl>,
}

impl Index {
    pub fn new(decls: Vec<Decl>) -> Index {
        Index { decls }
    }

    pub fn extend(&mut self, decls: impl IntoIterator<Item = Decl>) {
        self.decls.extend(decls);
    }

    pub fn len(&self) -> usize {
        self.decls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.decls.is_empty()
    }

    /// The best `limit` answers to `text`, best first, what the prelude
    /// provides winning a tie.
    pub fn search(&self, text: &str, limit: usize) -> Vec<Hit> {
        self.search_with(&Query::parse(text), limit, &|d| d.prelude)
    }

    /// The same, with the caller saying what should win a tie: the REPL knows
    /// exactly what is in scope, where the index only knows what a prelude
    /// provides.
    pub fn search_with(
        &self,
        query: &Query,
        limit: usize,
        prefer: &dyn Fn(&Decl) -> bool,
    ) -> Vec<Hit> {
        let mut hits = match query {
            Query::Empty => self
                .decls
                .iter()
                .enumerate()
                .map(|(i, _)| Hit { decl: i, score: 0 })
                .collect(),
            Query::Name(text) => self.by_name(text),
            Query::Type(sig) => self.by_type(sig),
        };
        hits.sort_by(|a, b| {
            let (x, y) = (&self.decls[a.decl], &self.decls[b.decl]);
            b.score
                .cmp(&a.score)
                .then(prefer(y).cmp(&prefer(x)))
                // Among equals, the shorter name is the likelier one -- the
                // `map` somebody wanted rather than `mapAccumL` -- and the
                // library's own modules before what a package adds.
                .then(x.name.len().cmp(&y.name.len()))
                .then(x.module.len().cmp(&y.module.len()))
                .then(x.name.cmp(&y.name))
                .then(x.module.cmp(&y.module))
        });
        hits.truncate(limit);
        hits
    }

    /// Fuzzy, against the name -- or against the qualified name, once the query
    /// has a `.` or a space in it and so says something about the module.
    fn by_name(&self, text: &str) -> Vec<Hit> {
        let pattern = Pattern::parse(text, CaseMatching::Smart, Normalization::Smart);
        let qualified = text.contains('.') || text.contains(' ');
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        self.decls
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let hay = if qualified {
                    d.qualified()
                } else {
                    d.name
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .to_string()
                };
                let score = pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher)?;
                // The name itself, exactly, above anything that merely
                // contains it -- and exactly as typed above exactly but for
                // case, so that `map` is the function before it is the type.
                let wanted = text.trim();
                let bonus = if qualified {
                    0
                } else if hay == wanted {
                    1 << 20
                } else if hay.eq_ignore_ascii_case(wanted) {
                    1 << 19
                } else {
                    0
                };
                Some(Hit {
                    decl: i,
                    score: score as i64 + bonus,
                })
            })
            .collect()
    }

    fn by_type(&self, sig: &shape::Sig) -> Vec<Hit> {
        self.decls
            .iter()
            .enumerate()
            .filter_map(|(i, d)| {
                let cost = shape::cost(sig, d.shape.as_ref()?)?;
                Some(Hit {
                    decl: i,
                    score: -(cost as i64),
                })
            })
            .collect()
    }
}

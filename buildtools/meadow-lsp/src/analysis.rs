//! Compiling one open document, and indexing the result by source position.
//!
//! The compiler is built around whole packages, not editors: it hands back a
//! `CompiledPackage` with a `TypeTable` keyed by `NodeId` and an HIR tree whose
//! nodes carry spans. Everything an editor asks — *what is the type here*, *where
//! is this defined* — is a lookup by position, so this module walks the tree once
//! and builds the indices the requests need.
//!
//! `Std` is compiled once and reused. It takes seconds, and a keystroke cannot
//! wait for it.

use meadow_compiler::{
    diagnostics::Diagnostic,
    hir::{self, VarId},
    infer::TypeTable,
    intern::InternedString,
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
    span::Span,
    AstModule, CompiledPackage, Options,
};

/// Where something is defined: which source, and where in it.
///
/// A [`Span`] is a pair of byte offsets and nothing more, so it only means
/// something paired with the text it indexes. Within one document that pairing
/// is implicit; across modules it has to be written down.
#[derive(Debug, Clone, Copy)]
pub struct Loc {
    pub source: Source,
    pub span: Span,
}

/// Every binding in scope, and where it was written.
///
/// This works across modules for one reason, and it is worth stating because it
/// is the whole trick: the resolver brings a dependency's exported binding into
/// scope **under that dependency's own `VarId`** (`meadow_rename::Resolver::import`).
/// So a mention of `concatAll` in someone's file already carries the id that
/// `Std.String` gave it. Nothing has to be re-resolved to follow it — the lookup
/// is the same one it always was, and only the *map* has to be bigger than a
/// single file.
pub type DefIndex = std::collections::HashMap<VarId, Loc>;

/// Types and data constructors, which have no id to be keyed by.
///
/// A `VarId` is minted by the resolver for every *value* binding. A type
/// constructor never gets one — [`hir::TypeExpr::Con`] keeps the name and
/// checks it against the tycon environment — and neither does a data
/// constructor, so both are matched the only way the compiler matches them
/// anywhere: by name.
///
/// That is not a shortcut taken here. `CompiledPackage::ctor_fields` is keyed
/// by bare constructor name, and so is the tag table the back end builds, so
/// two packages declaring the same constructor already collide long before an
/// editor looks at them. Keying this by name is exactly as correct as the rest
/// of the compiler, and no more. Giving type and data constructors real ids
/// would fix all three together; it is a change to the resolver, not to this.
///
/// Types and constructors are separate maps because one name can be both:
/// `data Pair = Pair Int Int` declares a type `Pair` and a constructor `Pair`,
/// and a reference knows which it meant from where it appears.
#[derive(Default)]
pub struct NameIndex {
    pub types: std::collections::HashMap<InternedString, Loc>,
    pub ctors: std::collections::HashMap<InternedString, Loc>,
}

impl NameIndex {
    fn get(&self, name: InternedString, ns: Namespace) -> Option<Loc> {
        match ns {
            Namespace::Type => self.types.get(&name),
            Namespace::Ctor => self.ctors.get(&name),
        }
        .copied()
    }

    fn absorb(&mut self, other: NameIndex) {
        self.types.extend(other.types);
        self.ctors.extend(other.ctors);
    }
}

/// One inlay hint: where it goes, and what it says.
///
/// The label is a *sequence* rather than a string because the type names in it
/// are meant to be clicked. Each is resolved to a location when the request is
/// answered, not here, because the wider index lives with the standard library
/// rather than with one document.
#[derive(Debug, Clone)]
pub struct Hint {
    /// Byte offset the hint is inserted at.
    pub offset: u32,
    pub parts: Vec<HintPart>,
}

#[derive(Debug, Clone)]
pub enum HintPart {
    /// Punctuation and spacing: `(`, ` : `, `)`.
    Text(String),
    /// A type or effect constructor — something to link.
    Name(InternedString),
}

/// Split a rendered type into clickable parts.
///
/// Deliberately a scan of the rendered text rather than a second renderer.
/// `meadow_infer::write_type` handles functions, effect rows, records and
/// precedence, and a copy of it here would drift the first time either changed.
/// The scan is sound because of a property of the syntax it produces: a
/// capitalised identifier in a rendered type is a type or effect constructor,
/// and a variable is always lowercase (the `Namer` hands out `a`, `b`, …). So
/// the capitalised runs are exactly the names worth linking, wherever they
/// appear and however they nest.
fn hint_parts(rendered: &str) -> Vec<HintPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut ident = String::new();

    let flush_ident = |ident: &mut String, text: &mut String, parts: &mut Vec<HintPart>| {
        if ident.is_empty() {
            return;
        }
        if ident.starts_with(|c: char| c.is_ascii_uppercase()) {
            if !text.is_empty() {
                parts.push(HintPart::Text(std::mem::take(text)));
            }
            parts.push(HintPart::Name(InternedString::from(ident.as_str())));
        } else {
            text.push_str(ident);
        }
        ident.clear();
    };

    for c in rendered.chars() {
        if c.is_alphanumeric() || c == '_' || c == '\'' {
            ident.push(c);
        } else {
            flush_ident(&mut ident, &mut text, &mut parts);
            text.push(c);
        }
    }
    flush_ident(&mut ident, &mut text, &mut parts);
    if !text.is_empty() {
        parts.push(HintPart::Text(text));
    }
    parts
}

/// Which of the two capitalised namespaces a name was written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// In a type position: `data Node a = Leaf (Vector a)`, or an effect label.
    Type,
    /// In an expression or a pattern: `Just x`.
    Ctor,
}

/// Everything the server knows about one document.
pub struct Analysis {
    pub source: String,
    pub diagnostics: Vec<Diagnostic>,
    /// Every typed node, innermost last — so a position lookup can take the
    /// *smallest* span that contains it by scanning for the tightest match.
    pub typed: Vec<(Span, String)>,
    /// Where each binding *in this document* is introduced. Local on purpose:
    /// [`Analysis::var_at`] matches these spans against this document's offsets,
    /// so a definition in another file has no business here. The wider index is
    /// [`Std::definitions`].
    pub defs: DefIndex,
    /// Every mention of a binding, for go-to-definition.
    pub refs: Vec<(Span, VarId)>,
    /// Every mention of a type or a data constructor, and which of the two it
    /// was -- the namespaces are separate, so the reference has to say.
    pub name_refs: Vec<(Span, InternedString, Namespace)>,
    /// Types and constructors declared *in this document*, for the same reason
    /// [`Analysis::defs`] is local.
    pub names: NameIndex,
    /// Inlay hints: parameters and `let`s, which carry no annotation in the
    /// source. Written to read as what you could have typed -- `(x : Int)` --
    /// because the annotation syntax is real and someone may want to keep it.
    pub binders: Vec<Hint>,
    /// Names of top-level bindings, for hover headers.
    pub binding_names: std::collections::HashMap<VarId, String>,
    /// Rendered scheme of each top-level binding.
    pub schemes: std::collections::HashMap<VarId, String>,
    /// Type names and constructor names, so highlighting can tell `Maybe` from
    /// `Just` — both are capitalised, and only resolution knows which is which.
    pub types_in_scope: std::collections::HashSet<String>,
    /// The throwaway [`Source`] this document was compiled as. A definition
    /// whose `Loc` names a different one is in another file.
    pub source_id: meadow_compiler::source::SourceId,
    pub ctors_in_scope: std::collections::HashSet<String>,
}

/// The standard library, compiled once.
pub struct Std {
    packages: Vec<CompiledPackage>,
    /// Each `Std` module as its own compiled unit, in dependency order, with the
    /// dotted name it is known by. Only needed to analyse a file that *is* one.
    modules: Vec<(String, CompiledPackage)>,
    types: std::collections::HashSet<String>,
    ctors: std::collections::HashSet<String>,
    /// Every binding the library defines, and where. Built once with the
    /// library, because it is the same walk over the same trees and the answer
    /// never changes.
    defs: DefIndex,
    /// The same for its types and data constructors.
    names: NameIndex,
    /// Where the embedded sources were written out, so a definition inside
    /// `Std` has a file an editor can open. `None` when nobody arranged it —
    /// go-to-definition then stops at the edge of the open document, which is
    /// what it did before this existed.
    src_root: Option<std::path::PathBuf>,
}

/// A `Std` module's path within the package.
///
/// `Lib` and `prelude` sit at the root — they are what a dependent gets
/// unqualified — and everything else is nested by its dotted name. It mirrors
/// `meadow::stdlib::module_path`, which this crate cannot call: `meadow` depends
/// on it for the `lsp` subcommand, so the arrow only points one way. The test
/// that analyses every module keeps the two honest.
fn module_path(dotted: &str) -> Vec<InternedString> {
    if dotted == "prelude" || dotted == "Lib" {
        Vec::new()
    } else {
        dotted.split('.').map(InternedString::from).collect()
    }
}

impl Std {
    /// Takes an already-compiled standard library. It is not loaded here on
    /// purpose: `meadow` owns the embedded sources and depends on this crate for
    /// its `lsp` subcommand, so reaching back for them would be a cycle.
    ///
    /// `modules` is the same library before bundling — see [`Std::module_at`].
    ///
    /// `src_root` is a directory holding the library's sources laid out by the
    /// relative names their [`SourceKind::File`] labels carry, so that a
    /// definition inside `Std` can be handed to an editor as a real file.
    pub fn new(
        packages: Vec<CompiledPackage>,
        modules: Vec<(String, CompiledPackage)>,
        src_root: Option<std::path::PathBuf>,
    ) -> Std {
        let mut types = std::collections::HashSet::new();
        let mut ctors = std::collections::HashSet::new();
        for p in &packages {
            collect_names(&p.data_decls, &mut types, &mut ctors);
        }
        // The bundle carries every module of the library, and it is the *same*
        // compile the pieces came from — `stdlib::std_packages` bundles what
        // `std_modules` produced — so one pass over it indexes both, and the
        // ids match either way a document was analysed.
        let mut defs = DefIndex::new();
        let mut names = NameIndex::default();
        for p in &packages {
            index_package(p, &mut defs, &mut names);
        }
        Std {
            packages,
            modules,
            types,
            ctors,
            defs,
            names,
            src_root,
        }
    }

    /// Every binding the standard library defines — the fallback when a name is
    /// in scope but was not written in the open document.
    pub fn definitions(&self) -> &DefIndex {
        &self.defs
    }

    /// Every type and data constructor the library declares, and where.
    pub fn declared_names(&self) -> &NameIndex {
        &self.names
    }

    /// A `file://` path for `source`, if one can be named.
    ///
    /// Modules discovered on disk carry an absolute path and need nothing.
    /// `Std`'s carry a *relative* label — `Std/Collections/Vector.mw` — because
    /// they were compiled from text baked into the binary, so they are resolved
    /// against wherever those sources were written out.
    pub fn path_of(&self, source: Source) -> Option<std::path::PathBuf> {
        let SourceKind::File(name) = source.kind else {
            return None;
        };
        let p = std::path::Path::new(&*name);
        if p.is_absolute() {
            return Some(p.to_path_buf());
        }
        let root = self.src_root.as_ref()?;
        let full = root.join(p);
        full.is_file().then_some(full)
    }

    /// Which `Std` module a document is, if it is one.
    ///
    /// `.../lib/Std/src/Collections/Vector.mw` is `Collections.Vector`. The name
    /// has to match one we were given, so a file that merely sits at a similar
    /// path in someone else's project is not mistaken for one.
    ///
    /// This matters because a `Std` module analysed the ordinary way — as a
    /// package depending on `Std` — declares every one of its own types a second
    /// time, and the editor fills with `already defined`.
    pub fn module_at(&self, uri: &str) -> Option<usize> {
        let path = uri.replace('\\', "/");
        let tail = path.rsplit_once("/Std/src/")?.1;
        let dotted = tail.strip_suffix(".mw")?.replace('/', ".");
        self.modules.iter().position(|(name, _)| *name == dotted)
    }

    /// The dotted name of the module at `index`, for reporting.
    pub fn module_name(&self, index: usize) -> &str {
        &self.modules[index].0
    }

    /// Compile `text` as a throwaway one-module package against `Std`.
    pub fn analyse(&self, text: &str) -> Analysis {
        let deps: Vec<&CompiledPackage> = self.packages.iter().collect();
        let name = InternedString::from("main");
        self.compile(text, name, name, Vec::new(), 1, &deps, None)
    }

    /// Compile `text` as the `Std` module it is: in its own place in the
    /// package, against the modules declared before it and nothing after.
    pub fn analyse_module(&self, index: usize, text: &str) -> Analysis {
        let (dotted, _) = &self.modules[index];
        let path = module_path(dotted);
        let unit = InternedString::from(dotted.as_str());
        let name = path.last().copied().unwrap_or(unit);
        let deps: Vec<&CompiledPackage> =
            self.modules[..index].iter().map(|(_, p)| p).collect();
        // Compiled *inside* the package, not merely against it. `Bool` says
        // `use Std.Test (assertEq)`, and a unit that does not know it belongs to
        // `Std` cannot resolve that.
        //
        // The package name comes off the bundle, whose `name` is the package;
        // a sub-unit's is its own unit name, which would make `Std.Test` look
        // like `Test.Test`.
        let package = self.packages.first().map(|p| p.name);
        self.compile(text, name, unit, path, index, &deps, package)
    }

    #[allow(clippy::too_many_arguments)]
    fn compile(
        &self,
        text: &str,
        name: InternedString,
        unit: InternedString,
        path: Vec<InternedString>,
        id: usize,
        deps: &[&CompiledPackage],
        package: Option<InternedString>,
    ) -> Analysis {
        let source = Source::new(SourceKind::Interactive, text.into());
        let lex = tokenize(source);
        let mut diagnostics = lex.errors;
        let (ast, perrs) = parser::parse(name, source, &lex.tokens);
        for e in &perrs {
            diagnostics.push(meadow_compiler::diagnostics::from_parse_error("main", e));
        }

        let modules = ast
            .map(|ast| vec![AstModule { path, name, ast, source }])
            .unwrap_or_default();

        let (pkg, mut unit_diags) = match package {
            Some(p) => meadow_compiler::compile_unit_in_package(
                p,
                unit,
                id,
                modules,
                deps,
                Options::debug(),
            ),
            None => meadow_compiler::compile_unit(name, id, modules, deps, Options::debug()),
        };
        diagnostics.append(&mut unit_diags);

        let mut a = Analysis {
            source: text.to_string(),
            source_id: source.id,
            diagnostics,
            typed: Vec::new(),
            defs: Default::default(),
            refs: Vec::new(),
            binders: Vec::new(),
            binding_names: Default::default(),
            schemes: Default::default(),
            name_refs: Vec::new(),
            names: Default::default(),
            types_in_scope: self.types.clone(),
            ctors_in_scope: self.ctors.clone(),
        };
        collect_names(
            &pkg.data_decls,
            &mut a.types_in_scope,
            &mut a.ctors_in_scope,
        );
        for e in &pkg.exports {
            a.binding_names.insert(e.var, e.name.to_string());
            a.schemes.insert(e.var, e.scheme.to_string());
        }
        for m in &pkg.modules {
            let mut w = Walk {
                types: Some(&pkg.types),
                source: m.source,
                namer: meadow_compiler::infer::Renderer::new(),
                a: &mut a,
            };
            w.module(&m.hir);
        }
        a
    }
}

/// Walk every module of `pkg` for its definitions alone.
///
/// Cheaper than it looks next to the full walk: rendering a type to a string is
/// what costs, and a package nobody has open needs none of them — so `types` is
/// `None` and `record` does nothing.
fn index_package(pkg: &CompiledPackage, into: &mut DefIndex, names: &mut NameIndex) {
    for m in &pkg.modules {
        let mut scratch = Analysis::empty();
        let mut w = Walk {
            types: None,
            source: m.source,
            namer: meadow_compiler::infer::Renderer::new(),
            a: &mut scratch,
        };
        w.module(&m.hir);
        into.extend(scratch.defs);
        names.absorb(scratch.names);
    }
}

struct Walk<'a> {
    /// `None` while indexing a dependency: it suppresses every `to_string` on a
    /// type, which is the whole cost of the walk.
    types: Option<&'a TypeTable>,
    /// What the spans this walk records are offsets into.
    source: Source,
    /// Names type variables for the hints of the declaration being walked.
    /// Shared across it so that one letter means one variable -- see
    /// [`meadow_compiler::infer::Renderer`].
    namer: meadow_compiler::infer::Renderer,
    a: &'a mut Analysis,
}

impl Walk<'_> {
    fn record(&mut self, span: Span, id: hir::NodeId) {
        if let Some(ty) = self.types.and_then(|t| t.get(id)) {
            let rendered = ty.to_string();
            self.a.typed.push((span, rendered));
        }
    }

    /// The type at `id`, named consistently with the rest of this declaration.
    fn hint_type(&mut self, id: hir::NodeId) -> Option<String> {
        let ty = self.types.and_then(|t| t.get(id))?.clone();
        Some(self.namer.render(&ty))
    }

    /// A binder: remember where it was introduced, and its type if there is one
    /// worth showing.
    fn bind(&mut self, ident: &hir::Ident, id: hir::NodeId, hint: bool) {
        let var = *ident.value();
        self.a.defs.insert(
            var,
            Loc {
                source: self.source,
                span: ident.span,
            },
        );
        if let Some(ty) = self.types.and_then(|t| t.get(id)) {
            self.a.typed.push((ident.span, ty.to_string()));
        }
        if let Some(rendered) = hint.then(|| self.hint_type(id)).flatten() {
            self.a.annotate(ident.span, &rendered);
        }
    }

    fn module(&mut self, m: &hir::LModule) {
        for d in &m.value().decls {
            self.decl(d);
        }
    }

    fn decl(&mut self, d: &hir::LDecl) {
        match d.value() {
            hir::Decl::Bind(b) => {
                // A fresh naming per declaration: `a` in one function has nothing
                // to do with `a` in the next, and sharing the namer across a
                // whole module would give unrelated functions different
                // letters for no reason.
                self.namer = meadow_compiler::infer::Renderer::new();
                self.bind_decl(b, false)
            }

            hir::Decl::Data(dd) => {
                self.declare(Namespace::Type, dd.name, dd.name_span);
                for v in &dd.variants {
                    self.declare(Namespace::Ctor, v.name, v.name_span);
                    match &v.fields {
                        hir::VariantFields::Positional(ts) => ts.iter().for_each(|t| self.ty(t)),
                        hir::VariantFields::Named(fs) => fs.iter().for_each(|(_, t)| self.ty(t)),
                    }
                }
            }
            hir::Decl::Record(rd) => {
                self.declare(Namespace::Type, rd.name, rd.name_span);
                rd.fields.iter().for_each(|(_, t)| self.ty(t));
            }
            hir::Decl::Effect(ed) => {
                self.declare(Namespace::Type, ed.name, ed.name_span);
                // An operation is callable as a value, so it has a `VarId` and
                // belongs in the ordinary definition index as well.
                for (_, ident, t) in &ed.ops {
                    self.bind(ident, ident.id, false);
                    self.ty(t);
                }
            }

            hir::Decl::Mod(_) | hir::Decl::Use(_) | hir::Decl::Error => {}
        }
    }

    /// A type or data constructor declaration.
    fn declare(&mut self, ns: Namespace, name: InternedString, span: Span) {
        let loc = Loc {
            source: self.source,
            span,
        };
        match ns {
            Namespace::Type => self.a.names.types.insert(name, loc),
            Namespace::Ctor => self.a.names.ctors.insert(name, loc),
        };
        // Standing on the declaration itself counts as referring to it, the way
        // it does for a value binding.
        self.a.name_refs.push((span, name, ns));
    }

    /// A resolved type expression: every constructor in it is a reference.
    fn ty(&mut self, t: &hir::LTypeExpr) {
        match t.value() {
            hir::TypeExpr::Con(name, args) => {
                self.a
                    .name_refs
                    .push((name.span, *name.value(), Namespace::Type));
                args.iter().for_each(|a| self.ty(a));
            }
            hir::TypeExpr::Fun(ps, r, eff) => {
                ps.iter().for_each(|p| self.ty(p));
                self.ty(r);
                if let Some(row) = eff {
                    for (label, args) in &row.labels {
                        args.iter().for_each(|a| self.ty(a));
                        let _ = label;
                    }
                }
            }
            hir::TypeExpr::Tuple(ts) => ts.iter().for_each(|x| self.ty(x)),
            hir::TypeExpr::Vector(x) | hir::TypeExpr::List(x) => self.ty(x),
            hir::TypeExpr::Var(_) => {}
        }
    }

    /// `hint` is false at the top level: a `def`'s own name already shows its
    /// scheme on hover, and an inlay there would only repeat the signature.
    fn bind_decl(&mut self, b: &hir::Bind, hint: bool) {
        match b {
            hir::Bind::Fun(name, params, declared, body) => {
                self.bind(name, name.id, false);
                for p in params {
                    self.pat(p, true);
                }
                if let Some(t) = declared {
                    self.ty(t);
                }
                self.expr(body);
                // The result type, which nothing else shows: a top-level
                // binding hovers as its scheme, but a reader wants it on the
                // line. Only where there is a parameter list to put it after.
                // ...unless it is already written, in which case the hint
                // would be repeating what is on the line.
                if declared.is_none() {
                    if let (Some(last), Some(rendered)) =
                        (params.last(), self.hint_type(body.id))
                    {
                        self.a.annotate_result(last.span, &rendered);
                    }
                }
            }
            hir::Bind::Pat(pat, expr) => {
                self.pat(pat, hint);
                self.expr(expr);
            }
            hir::Bind::Error => {}
        }
    }

    fn pat(&mut self, p: &hir::LPat, hint: bool) {
        self.record(p.span, p.id);
        match p.value() {
            hir::Pat::Var(ident) => self.bind(ident, ident.id, hint),
            // The annotation is written in the source, so it is a type
            // *reference* -- and the pattern under it needs no inlay hint,
            // because the type is already there to read.
            hir::Pat::Ann(inner, t) => {
                self.pat(inner, false);
                self.ty(t);
            }
            hir::Pat::As(ident, sub) => {
                self.bind(ident, ident.id, hint);
                self.pat(sub, false);
            }
            hir::Pat::Tuple(ps)
            | hir::Pat::List(ps)
            | hir::Pat::Array(ps) => ps.iter().for_each(|q| self.pat(q, hint)),
            hir::Pat::Cons(name, ps) => {
                self.a
                    .name_refs
                    .push((name.span, *name.value(), Namespace::Ctor));
                ps.iter().for_each(|q| self.pat(q, hint));
            }
            hir::Pat::Record(fs, _) => fs.iter().for_each(|(_, q)| self.pat(q, hint)),
            hir::Pat::Wildcard | hir::Pat::Lit(_) | hir::Pat::Unit | hir::Pat::Error => {}
        }
    }

    fn expr(&mut self, e: &hir::LExpr) {
        self.record(e.span, e.id);
        match e.value() {
            hir::Expr::Var(ident) => {
                self.a.refs.push((ident.span, *ident.value()));
                self.record(ident.span, ident.id);
            }
            hir::Expr::Lam(params, body) => {
                params.iter().for_each(|p| self.pat(p, true));
                self.expr(body);
            }
            hir::Expr::App(f, args) => {
                self.expr(f);
                args.iter().for_each(|a| self.expr(a));
            }
            hir::Expr::Let(binds, body) => {
                binds.iter().for_each(|b| self.bind_decl(b, true));
                self.expr(body);
            }
            hir::Expr::If(c, t, f) => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            hir::Expr::Match(scrut, arms) => {
                self.expr(scrut);
                for (p, arm) in arms {
                    self.pat(p, true);
                    self.expr(arm);
                }
            }
            hir::Expr::Tuple(xs) | hir::Expr::List(xs) | hir::Expr::Array(xs) => {
                xs.iter().for_each(|x| self.expr(x))
            }
            hir::Expr::Cons(name, xs) => {
                self.a
                    .name_refs
                    .push((name.span, *name.value(), Namespace::Ctor));
                xs.iter().for_each(|x| self.expr(x));
            }
            hir::Expr::Record(fs, base) => {
                fs.iter().for_each(|(_, x)| self.expr(x));
                if let Some(b) = base {
                    self.expr(b);
                }
            }
            hir::Expr::Field(x, _) => self.expr(x),
            hir::Expr::Handle(body, arms, ret) => {
                self.expr(body);
                for arm in arms {
                    self.pat(&arm.param, true);
                    self.bind(&arm.resume, arm.resume.id, true);
                    self.expr(&arm.body);
                }
                if let Some((p, b)) = ret {
                    self.pat(p, true);
                    self.expr(b);
                }
            }
            hir::Expr::Lit(_) | hir::Expr::Unit | hir::Expr::Error => {}
        }
    }
}

// --- queries -----------------------------------------------------------------

impl Analysis {
    /// The type of the smallest node covering `offset`.
    ///
    /// Spans nest, so the tightest one wins: at the `x` in `f x`, that is `x`'s
    /// own type rather than the whole application's.
    pub fn type_at(&self, offset: usize) -> Option<&str> {
        self.typed
            .iter()
            .filter(|(s, _)| covers(*s, offset))
            .min_by_key(|(s, _)| s.end - s.start)
            .map(|(_, t)| t.as_str())
    }

    /// The binding mentioned at `offset`, if any.
    ///
    /// The smallest covering reference wins. An operator application records the
    /// operator itself over the *whole* expression — `answer + 1` is a mention of
    /// `+` spanning all of it — so the first match is routinely the wrong one.
    /// Among equally small candidates, prefer one defined in this file, since
    /// that is the one go-to-definition can actually take you to.
    pub fn var_at(&self, offset: usize) -> Option<VarId> {
        self.refs
            .iter()
            .filter(|(s, _)| covers(*s, offset))
            .min_by_key(|(s, v)| (s.end - s.start, !self.defs.contains_key(v)))
            .map(|(_, v)| *v)
            .or_else(|| {
                // Standing on the definition itself counts as referring to it.
                self.defs
                    .iter()
                    .filter(|(_, l)| covers(l.span, offset))
                    .min_by_key(|(_, l)| l.span.end - l.span.start)
                    .map(|(v, _)| *v)
            })
    }

    /// Where the binding under the cursor was introduced.
    ///
    /// `fallback` covers everything this document depends on. It is consulted
    /// second and is a plain map lookup, because a reference to an imported
    /// name already carries the defining module's `VarId` -- see [`DefIndex`].
    pub fn definition_at(
        &self,
        offset: usize,
        fallback: &DefIndex,
        declared: &NameIndex,
    ) -> Option<Loc> {
        // A value binding first: it is the only one of the three that is matched
        // by a resolved id rather than by a name, so when it answers, it is the
        // answer.
        if let Some(var) = self.var_at(offset) {
            // This document before the wider index: a local binding shadows an
            // imported one, and it is the local one the reference resolved to.
            if let Some(loc) = self.defs.get(&var).or_else(|| fallback.get(&var)) {
                return Some(*loc);
            }
        }
        // Then a type or a data constructor. Innermost wins, so `Int` inside
        // `Maybe Int` beats the `Maybe` whose span encloses it.
        let (_, name, ns) = self
            .name_refs
            .iter()
            .filter(|(s, _, _)| covers(*s, offset))
            .min_by_key(|(s, _, _)| s.end - s.start)?;
        self.names.get(*name, *ns).or_else(|| declared.get(*name, *ns))
    }

    /// Markdown for the hover: a signature, then any doc comment above it.
    pub fn hover_at(&self, offset: usize) -> Option<String> {
        let var = self.var_at(offset);
        let signature = match var.and_then(|v| self.schemes.get(&v)) {
            // A top-level binding shows its generalised scheme, which is more
            // informative than the type at this particular use site.
            Some(scheme) => {
                let name = var.and_then(|v| self.binding_names.get(&v));
                match name {
                    Some(n) => format!("{n} : {scheme}"),
                    None => scheme.clone(),
                }
            }
            None => {
                let ty = self.type_at(offset)?;
                match var.and_then(|v| self.binding_names.get(&v)) {
                    Some(n) => format!("{n} : {ty}"),
                    None => ty.to_string(),
                }
            }
        };

        let mut out = format!("```meadow\n{signature}\n```");
        if let Some(doc) = var.and_then(|v| self.defs.get(&v))
            .and_then(|l| self.doc_above(l.span)) {
            out.push_str("\n\n---\n\n");
            out.push_str(&doc);
        }
        Some(out)
    }

    /// The `--` comment block immediately above the line `span` starts on.
    ///
    /// Comments are discarded by the lexer, so documentation is recovered from
    /// the source text — the run of comment lines directly above a definition,
    /// with the leading `--` stripped.
    pub fn doc_above(&self, span: Span) -> Option<String> {
        let before = self.source.get(..span.start as usize)?;
        let mut lines: Vec<&str> = Vec::new();
        // The line the definition starts on is partial; skip it.
        for line in before.lines().rev().skip(1) {
            let t = line.trim();
            if t.is_empty() && lines.is_empty() {
                continue;
            }
            match t.strip_prefix("--") {
                Some(rest) => lines.push(rest.trim()),
                None => break,
            }
        }
        if lines.is_empty() {
            return None;
        }
        lines.reverse();
        Some(lines.join("\n"))
    }
}

fn covers(s: Span, offset: usize) -> bool {
    (s.start as usize) <= offset && offset < (s.end as usize)
}

/// Type and constructor names declared by `decls`.
fn collect_names(
    decls: &[hir::LDecl],
    types: &mut std::collections::HashSet<String>,
    ctors: &mut std::collections::HashSet<String>,
) {
    for d in decls {
        match d.value() {
            hir::Decl::Data(dd) => {
                types.insert(dd.name.to_string());
                ctors.extend(dd.variants.iter().map(|v| v.name.to_string()));
            }
            hir::Decl::Record(rd) => {
                types.insert(rd.name.to_string());
                ctors.insert(rd.name.to_string());
            }
            hir::Decl::Effect(ed) => {
                types.insert(ed.name.to_string());
            }
            _ => {}
        }
    }
}

impl Analysis {
    /// An empty analysis, for a walk that only wants the definitions out of it.
    fn empty() -> Analysis {
        Analysis {
            source: String::new(),
            diagnostics: Vec::new(),
            typed: Vec::new(),
            defs: Default::default(),
            refs: Vec::new(),
            name_refs: Vec::new(),
            names: Default::default(),
            binders: Vec::new(),
            binding_names: Default::default(),
            schemes: Default::default(),
            types_in_scope: Default::default(),
            ctors_in_scope: Default::default(),
            source_id: 0,
        }
    }
}

impl Analysis {
    /// Hint `(name : ty)` around the binder at `span`.
    ///
    /// Two hints, not one: an opening parenthesis before the name and the
    /// annotation after it. The parentheses are not decoration — `(x : Int)` is
    /// the syntax, so the hint reads as something that could be typed in its
    /// place rather than as a notation of its own.
    fn annotate(&mut self, span: Span, rendered: &str) {
        self.binders.push(Hint {
            offset: span.start,
            parts: vec![HintPart::Text("(".to_string())],
        });
        let mut parts = vec![HintPart::Text(" : ".to_string())];
        parts.extend(hint_parts(rendered));
        parts.push(HintPart::Text(")".to_string()));
        self.binders.push(Hint {
            offset: span.end,
            parts,
        });
    }

    /// Hint the result type of a function, after its last parameter.
    ///
    /// Appended to the hint already sitting there when there is one, so that
    /// `fun add x y` reads `fun add (x : Int) (y : Int) : Int` rather than
    /// producing two hints at one position whose order is the client's to
    /// decide. A parameter that is itself parenthesised — `fun f (a, b)` —
    /// closes before its own span ends, so nothing is there to append to and
    /// the result gets a hint of its own.
    fn annotate_result(&mut self, after: Span, rendered: &str) {
        let mut parts = vec![HintPart::Text(" : ".to_string())];
        parts.extend(hint_parts(rendered));
        match self.binders.iter_mut().find(|h| h.offset == after.end) {
            Some(h) => h.parts.extend(parts),
            None => self.binders.push(Hint {
                offset: after.end,
                parts,
            }),
        }
    }
}

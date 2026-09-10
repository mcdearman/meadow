//! The embedded **`Std`** package.
//!
//! `lib/Std/` on disk is the source of truth; its modules are `include_str!`d here
//! so the compiler always has them, with no filesystem lookup. [`compile_std`]
//! builds them into a single [`CompiledPackage`] that [`crate::pipeline::build`]
//! and the `meadow` crate inject as an implicit dependency of everything else — so the
//! prelude's names (`map`, `foldl`, `Option`, `Bool`, …) are in scope everywhere
//! without an explicit `use`.
//!
//! There is no qualified `use` yet, so every top-level name across these modules
//! shares one flat namespace and must be unique. Each module marks its public API
//! `@pub`; `prelude` defines nothing and only `@pub use`-re-exports a curated set
//! (including `map` / `filter` / `foldl` / `foldr` / … which are deliberately
//! *not* `@pub` in `Std.Collections.List`, so re-export is what makes them
//! public).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use meadow_compiler::{
    compile_unit_in_package,
    diagnostics::{self, Diagnostic},
    infer::TypeTable,
    intern::InternedString,
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
    AstModule, CompiledPackage, Export, Options,
};
use std::collections::HashMap;

/// The package name shown in `:module` / linker dumps, and the `Std` in
/// `Std.Collections.List`.
pub const PACKAGE_NAME: &str = "Std";

/// `(dotted module path, source)`. Each is compiled as its own unit, in this
/// order, with the earlier ones as dependencies — a module sees another only
/// through an explicit `use`. `prelude` is last and only re-exports; its
/// re-exports are the names a dependent package gets unqualified.
pub const MODULES: &[(&str, &str)] = &[
    ("Lib", include_str!("../../../lib/Std/src/Lib.mw")),
    // First, and dependency-free, so every module below can hold `@test`s.
    ("Test", include_str!("../../../lib/Std/src/Test.mw")),
    ("Bool", include_str!("../../../lib/Std/src/Bool.mw")),
    ("Ref", include_str!("../../../lib/Std/src/Ref.mw")),
    ("Ordering", include_str!("../../../lib/Std/src/Ordering.mw")),
    ("Function", include_str!("../../../lib/Std/src/Function.mw")),
    ("Tuple", include_str!("../../../lib/Std/src/Tuple.mw")),
    ("Int", include_str!("../../../lib/Std/src/Int.mw")),
    ("Maybe", include_str!("../../../lib/Std/src/Maybe.mw")),
    ("Char", include_str!("../../../lib/Std/src/Char.mw")),
    ("Result", include_str!("../../../lib/Std/src/Result.mw")),
    ("Either", include_str!("../../../lib/Std/src/Either.mw")),
    ("Bits", include_str!("../../../lib/Std/src/Bits.mw")),
    ("Bytes", include_str!("../../../lib/Std/src/Bytes.mw")),
    ("Yield", include_str!("../../../lib/Std/src/Yield.mw")),
    ("Collections", include_str!("../../../lib/Std/src/Collections.mw")),
    ("Collections.Vector", include_str!("../../../lib/Std/src/Collections/Vector.mw")),
    ("Collections.List", include_str!("../../../lib/Std/src/Collections/List.mw")),
    ("Collections.Tree", include_str!("../../../lib/Std/src/Collections/Tree.mw")),
    ("Collections.Set", include_str!("../../../lib/Std/src/Collections/Set.mw")),
    ("Collections.Map", include_str!("../../../lib/Std/src/Collections/Map.mw")),
    ("Sort", include_str!("../../../lib/Std/src/Sort.mw")),
    ("State", include_str!("../../../lib/Std/src/State.mw")),
    ("Exn", include_str!("../../../lib/Std/src/Exn.mw")),
    ("Stream", include_str!("../../../lib/Std/src/Stream.mw")),
    ("Random", include_str!("../../../lib/Std/src/Random.mw")),
    ("Fs", include_str!("../../../lib/Std/src/Fs.mw")),
    ("Process", include_str!("../../../lib/Std/src/Process.mw")),
    ("String", include_str!("../../../lib/Std/src/String.mw")),
    ("String.Parse", include_str!("../../../lib/Std/src/String/Parse.mw")),
    ("Path", include_str!("../../../lib/Std/src/Path.mw")),
    ("Json", include_str!("../../../lib/Std/src/Json.mw")),
    ("Time", include_str!("../../../lib/Std/src/Time.mw")),
    ("Console", include_str!("../../../lib/Std/src/Console.mw")),
    ("Bench", include_str!("../../../lib/Std/src/Bench.mw")),
    ("prelude", include_str!("../../../lib/Std/src/prelude.mw")),
];

/// Each `Std` module compiled as its own unit, in dependency order, paired with
/// the dotted name `MODULES` spells it with.
///
/// [`std_packages`] is the bundle a dependent sees; this is what that bundle was
/// made from. The language server needs the pieces: a file that *is* a `Std`
/// module has to be analysed in its own place — against the modules before it —
/// or every type and constructor it declares collides with the copy sitting in
/// its own dependency, and the editor fills with `already defined`.
pub fn std_modules(opts: Options) -> (Vec<(&'static str, CompiledPackage)>, Vec<Diagnostic>) {
    type Cache = OnceLock<(Vec<(&'static str, CompiledPackage)>, Vec<Diagnostic>)>;
    static DEBUG: Cache = OnceLock::new();
    static RELEASE: Cache = OnceLock::new();
    let cell = if opts.check_exhaustive { &RELEASE } else { &DEBUG };
    let (modules, diags) = cell.get_or_init(|| {
        counter(opts).fetch_add(1, Ordering::Relaxed);
        compile_modules(opts)
    });
    (modules.clone(), diags.clone())
}

/// The embedded `Std` package, compiled once per process.
///
/// [`compile_modules`] takes about five seconds and is deterministic, so
/// compiling it twice in one process is pure waste — and something did exactly
/// that on every REPL line, every language-server keystroke, and once per test,
/// which is what made the test suite take minutes rather than seconds.
///
/// Cached per profile: `--release` turns on the exhaustiveness check and can
/// report different diagnostics. The result is cloned rather than shared,
/// because linking consumes its packages — and cloning the compiled tree is
/// about two orders of magnitude cheaper than rebuilding it.
pub fn std_packages(opts: Options) -> (Vec<CompiledPackage>, Vec<Diagnostic>) {
    static DEBUG: OnceLock<(Vec<CompiledPackage>, Vec<Diagnostic>)> = OnceLock::new();
    static RELEASE: OnceLock<(Vec<CompiledPackage>, Vec<Diagnostic>)> = OnceLock::new();
    let cell = if opts.check_exhaustive { &RELEASE } else { &DEBUG };
    let (packages, diags) = cell.get_or_init(|| {
        // Shares the one compile with `std_modules`, so asking for both costs
        // memory but not time.
        let (modules, diags) = std_modules(opts);
        let subs = modules.into_iter().map(|(_, p)| p).collect();
        (
            vec![bundle(InternedString::from(PACKAGE_NAME), subs)],
            diags,
        )
    });
    (packages.clone(), diags.clone())
}

/// The module path a dotted name sits at within the package.
///
/// `Lib` and `prelude` are at the root: they are what a dependent gets
/// unqualified. Everything else is nested, so a sibling reaches it only through
/// an explicit `use`.
pub fn module_path(dotted: &str) -> Vec<InternedString> {
    if dotted == "prelude" || dotted == "Lib" {
        Vec::new()
    } else {
        dotted.split('.').map(InternedString::from).collect()
    }
}

fn counter(opts: Options) -> &'static AtomicUsize {
    static DEBUG: AtomicUsize = AtomicUsize::new(0);
    static RELEASE: AtomicUsize = AtomicUsize::new(0);
    if opts.check_exhaustive { &RELEASE } else { &DEBUG }
}

/// How many times [`std_packages`] has actually compiled `Std` for this profile.
///
/// At most one, for the life of the process — that is the whole point of the
/// cache, and it is what `tests/stdlib.rs` asserts. It used to assert it by
/// timing two calls against each other, which is not a fact about the cache: the
/// tests in that binary run in parallel and most of them warm it first, so the
/// "cold" measurement was a second clone, and on a loaded machine the two were
/// indistinguishable noise. A count is the property itself.
pub fn compiles(opts: Options) -> usize {
    counter(opts).load(Ordering::Relaxed)
}

/// Compile each embedded `Std` module as its own unit, in dependency order.
///
/// Returns them paired with their dotted names plus any diagnostics — which for
/// a healthy tree is empty. Callers surface the diagnostics
/// ([`crate::pipeline::build`] and the REPL print them; `tests/stdlib.rs` asserts
/// they stay empty).
fn compile_modules(opts: Options) -> (Vec<(&'static str, CompiledPackage)>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let pkg = InternedString::from(PACKAGE_NAME);

    // Compile each module as its own unit, in order, with the ones already done as
    // dependencies. Each sub-unit gets `prelude_exports = Some([])` so a later
    // sibling only reaches it through `use`.
    let mut subs: Vec<(&'static str, CompiledPackage)> = Vec::new();
    for (dotted, src) in MODULES {
        let filename = format!("Std/{}.mw", dotted.replace('.', "/"));
        let source = Source::new(
            SourceKind::File(InternedString::from(filename.as_str())),
            InternedString::from(*src),
        );
        let lex = tokenize(source);
        diags.extend(lex.errors);

        let path = module_path(dotted);
        let mname = path.last().copied().unwrap_or_else(|| InternedString::from(*dotted));

        let (ast, perrs) = parser::parse(mname, source, &lex.tokens);
        for e in &perrs {
            diags.push(diagnostics::from_parse_error(&filename, e));
        }
        let Some(ast) = ast else { continue };

        let dep_refs: Vec<&CompiledPackage> = subs.iter().map(|(_, p)| p).collect();
        let (mut cp, unit_diags) = compile_unit_in_package(
            pkg,
            InternedString::from(*dotted),
            subs.len(),
            vec![AstModule { path: path.clone(), name: mname, ast }],
            &dep_refs,
            opts,
        );
        diags.extend(unit_diags);
        // A sibling reaches this module only via `use`, never flat.
        cp.prelude_exports = Some(Vec::new());
        // Tag exports with the module path so `use Std.a.b (x)` from outside works.
        for e in &mut cp.exports {
            e.module = path.clone();
        }
        subs.push((dotted, cp));
    }

    (subs, diags)
}

/// Fold the separately-compiled `Std` modules into one package. The `prelude`
/// module's exports (path `[]`) become the names a dependent gets unqualified.
fn bundle(name: InternedString, subs: Vec<CompiledPackage>) -> CompiledPackage {
    let mut defs = Vec::new();
    let mut modules = Vec::new();
    let mut ctor_fields = HashMap::new();
    let mut data_decls = Vec::new();
    let mut types = TypeTable::default();
    let mut exports: Vec<Export> = Vec::new();
    let mut prelude_names: Vec<InternedString> = Vec::new();
    let mut tests = Vec::new();

    for sub in subs {
        defs.extend(sub.defs);
        modules.extend(sub.modules);
        ctor_fields.extend(sub.ctor_fields);
        data_decls.extend(sub.data_decls);
        tests.extend(sub.tests);
        types.absorb(sub.types);
        for e in sub.exports {
            if e.module.is_empty() {
                prelude_names.push(e.name);
            }
            exports.push(e);
        }
    }

    CompiledPackage {
        id: 0,
        name,
        modules,
        types,
        exports,
        defs,
        ctor_fields,
        data_decls,
        tests,
        prelude_exports: Some(prelude_names),
    }
}

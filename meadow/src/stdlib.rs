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
const MODULES: &[(&str, &str)] = &[
    ("Lib", include_str!("../../lib/Std/src/Lib.mw")),
    ("Bool", include_str!("../../lib/Std/src/Bool.mw")),
    ("Ordering", include_str!("../../lib/Std/src/Ordering.mw")),
    ("Function", include_str!("../../lib/Std/src/Function.mw")),
    ("Tuple", include_str!("../../lib/Std/src/Tuple.mw")),
    ("Int", include_str!("../../lib/Std/src/Int.mw")),
    ("Maybe", include_str!("../../lib/Std/src/Maybe.mw")),
    ("Result", include_str!("../../lib/Std/src/Result.mw")),
    ("Bits", include_str!("../../lib/Std/src/Bits.mw")),
    ("Bytes", include_str!("../../lib/Std/src/Bytes.mw")),
    ("Collections", include_str!("../../lib/Std/src/Collections.mw")),
    ("Collections.Vector", include_str!("../../lib/Std/src/Collections/Vector.mw")),
    ("Collections.List", include_str!("../../lib/Std/src/Collections/List.mw")),
    ("Collections.Tree", include_str!("../../lib/Std/src/Collections/Tree.mw")),
    ("Collections.Set", include_str!("../../lib/Std/src/Collections/Set.mw")),
    ("Collections.Map", include_str!("../../lib/Std/src/Collections/Map.mw")),
    ("Fs", include_str!("../../lib/Std/src/Fs.mw")),
    ("Process", include_str!("../../lib/Std/src/Process.mw")),
    ("String", include_str!("../../lib/Std/src/String.mw")),
    ("String.Parse", include_str!("../../lib/Std/src/String/Parse.mw")),
    ("prelude", include_str!("../../lib/Std/src/prelude.mw")),
];

/// Compile the embedded `Std` package.
///
/// Returns the one compiled package plus any diagnostics — which for a healthy
/// tree is empty. Callers surface the diagnostics ([`crate::pipeline::build`] and
/// the REPL print them; `tests/stdlib.rs` asserts they stay empty).
pub fn compile_std(opts: Options) -> (Vec<CompiledPackage>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let pkg = InternedString::from(PACKAGE_NAME);

    // Compile each module as its own unit, in order, with the ones already done as
    // dependencies. Each sub-unit gets `prelude_exports = Some([])` so a later
    // sibling only reaches it through `use`.
    let mut subs: Vec<CompiledPackage> = Vec::new();
    for (dotted, src) in MODULES {
        let filename = format!("Std/{}.mw", dotted.replace('.', "/"));
        let source = Source::new(
            SourceKind::File(InternedString::from(filename.as_str())),
            InternedString::from(*src),
        );
        let lex = tokenize(source);
        diags.extend(lex.errors);

        let path: Vec<InternedString> = if *dotted == "prelude" || *dotted == "Lib" {
            Vec::new()
        } else {
            dotted.split('.').map(InternedString::from).collect()
        };
        let mname = path.last().copied().unwrap_or_else(|| InternedString::from(*dotted));

        let (ast, perrs) = parser::parse(mname, source, &lex.tokens);
        for e in &perrs {
            diags.push(diagnostics::from_parse_error(&filename, e));
        }
        let Some(ast) = ast else { continue };

        let dep_refs: Vec<&CompiledPackage> = subs.iter().collect();
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
        subs.push(cp);
    }

    (vec![bundle(pkg, subs)], diags)
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

    for sub in subs {
        defs.extend(sub.defs);
        modules.extend(sub.modules);
        ctor_fields.extend(sub.ctor_fields);
        data_decls.extend(sub.data_decls);
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
        prelude_exports: Some(prelude_names),
    }
}

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
    compile_unit,
    diagnostics::{self, Diagnostic},
    intern::InternedString,
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
    AstModule, CompiledPackage,
};

/// The package name shown in `:module` / linker dumps, and the `Std` in
/// `Std.Collections.List`.
pub const PACKAGE_NAME: &str = "Std";

/// `(dotted module path, source)`. Order matters: a module may only refer to
/// names from itself or an earlier module (inference has no cross-binding
/// pre-pass), so leaves come first and `prelude` (which only re-exports) last.
const MODULES: &[(&str, &str)] = &[
    ("Bool", include_str!("../../lib/Std/src/Bool.mw")),
    ("Ordering", include_str!("../../lib/Std/src/Ordering.mw")),
    ("Function", include_str!("../../lib/Std/src/Function.mw")),
    ("Tuple", include_str!("../../lib/Std/src/Tuple.mw")),
    ("Int", include_str!("../../lib/Std/src/Int.mw")),
    ("Option", include_str!("../../lib/Std/src/Option.mw")),
    ("Result", include_str!("../../lib/Std/src/Result.mw")),
    ("Collections.List", include_str!("../../lib/Std/src/Collections/List.mw")),
    ("Collections.Tree", include_str!("../../lib/Std/src/Collections/Tree.mw")),
    ("Collections.Set", include_str!("../../lib/Std/src/Collections/Set.mw")),
    ("Collections.Map", include_str!("../../lib/Std/src/Collections/Map.mw")),
    ("Fs", include_str!("../../lib/Std/src/Fs.mw")),
    ("prelude", include_str!("../../lib/Std/src/prelude.mw")),
];

/// Compile the embedded `Std` package.
///
/// Returns the one compiled package plus any diagnostics — which for a healthy
/// tree is empty. Callers surface the diagnostics ([`crate::pipeline::build`] and
/// the REPL print them; `tests/stdlib.rs` asserts they stay empty).
pub fn compile_std() -> (Vec<CompiledPackage>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut modules = Vec::new();

    for (dotted, src) in MODULES {
        let filename = format!("Std/{}.mw", dotted.replace('.', "/"));
        let source = Source::new(
            SourceKind::File(InternedString::from(filename.as_str())),
            InternedString::from(*src),
        );
        let lex = tokenize(source);
        diags.extend(lex.errors);

        // `Collections.Map` -> path `[Collections, Map]`, name `Map`.
        let path: Vec<InternedString> = if *dotted == "prelude" {
            Vec::new()
        } else {
            dotted.split('.').map(InternedString::from).collect()
        };
        let mname = path.last().copied().unwrap_or_else(|| InternedString::from(*dotted));

        let (ast, perrs) = parser::parse(mname, source, &lex.tokens);
        for e in &perrs {
            diags.push(diagnostics::from_parse_error(&filename, e));
        }
        if let Some(ast) = ast {
            modules.push(AstModule {
                path,
                name: mname,
                ast,
            });
        }
    }

    let (cp, unit_diags) =
        compile_unit(InternedString::from(PACKAGE_NAME), 0, modules, &[]);
    diags.extend(unit_diags);
    (vec![cp], diags)
}

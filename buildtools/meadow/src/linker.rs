//! The linker stitches compiled packages into one program.
//!
//! With no code generation yet its job is modest: concatenate every package's
//! [`core::Def`]s in dependency order, build one global symbol table, and locate
//! the `main` entry point. It also keeps the per-package type tables around so the
//! driver can print a fully annotated tree.

use meadow_compiler::{core, hir::VarId, infer::Scheme, intern::InternedString, CompiledPackage};
use std::fmt::Write;

pub struct GlobalSymbol {
    pub package: InternedString,
    pub name: InternedString,
    pub var: VarId,
    pub scheme: Scheme,
}

/// A `@test` function, and the package and module it came from.
pub struct TestCase {
    pub package: InternedString,
    /// `Module.name`, or the bare name in the root module -- see
    /// [`meadow_compiler::TestSite::qualified`]. What `meadow test` prints and
    /// filters on.
    pub name: InternedString,
    pub var: VarId,
}

pub struct LinkedProgram {
    pub program: core::Program,
    pub symbols: Vec<GlobalSymbol>,
    pub packages: Vec<CompiledPackage>,
    /// Every `@test` in the linked packages, in package then declaration order.
    pub tests: Vec<TestCase>,
}

pub struct Linker;

impl Linker {
    pub fn link(packages: Vec<CompiledPackage>) -> LinkedProgram {
        let mut defs = Vec::new();
        let mut symbols = Vec::new();
        let mut tests = Vec::new();
        let mut ctor_fields = std::collections::HashMap::new();
        let mut entry = None;

        for pkg in &packages {
            defs.extend(pkg.defs.iter().cloned());
            ctor_fields.extend(pkg.ctor_fields.clone());
            tests.extend(pkg.test_sites().into_iter().map(|t| TestCase {
                package: pkg.name,
                name: InternedString::from(t.qualified()),
                var: t.var,
            }));
            // A package's own `main`, which needs no export — see
            // `CompiledPackage::entry`. The last one wins, and packages arrive
            // in dependency order, so that is the root package's.
            if let Some(var) = pkg.entry {
                entry = Some(var);
            }
            for e in &pkg.exports {
                if &*e.name == "main" {
                    entry = Some(e.var);
                }
                symbols.push(GlobalSymbol {
                    package: pkg.name,
                    name: e.name,
                    var: e.var,
                    scheme: e.scheme.clone(),
                });
            }
        }

        LinkedProgram {
            program: core::Program {
                defs,
                entry,
                ctor_fields,
            },
            symbols,
            packages,
            tests,
        }
    }
}

impl LinkedProgram {
    /// Human-readable summary: every exported symbol with its inferred scheme, plus
    /// how many HIR nodes each package managed to annotate, plus the entry point.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for pkg in &self.packages {
            let _ = writeln!(out, "=== package {} ===", pkg.name);
            for sym in self.symbols.iter().filter(|s| s.package == pkg.name) {
                let _ = writeln!(out, "  {} : {}", sym.name, sym.scheme);
            }
            let annotated = pkg
                .types
                .rendered()
                .len();
            let _ = writeln!(out, "  ({annotated} annotated nodes)");
        }
        match self.program.entry {
            Some(v) => {
                // An entry point need not be exported, so the symbol table
                // may not know it; it is `main` either way.
                let name = self
                    .symbols
                    .iter()
                    .find(|s| s.var == v)
                    .map(|s| s.name.to_string())
                    .unwrap_or_else(|| "main".to_string());
                let _ = writeln!(out, "entry: {name}");
            }
            None => {
                let _ = writeln!(out, "entry: (none)");
            }
        }
        out
    }

    /// Every annotated node, as `pkg#id : type`. Useful for eyeballing the fully
    /// typed tree.
    pub fn annotations(&self) -> String {
        let mut out = String::new();
        for pkg in &self.packages {
            for (id, ty) in pkg.types.rendered() {
                let _ = writeln!(out, "{}#{} : {}", pkg.name, id.0, ty);
            }
        }
        out
    }
}

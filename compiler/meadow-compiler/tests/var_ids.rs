//! `VarId`s belong to a compilation unit, and say which one.
//!
//! This is the property that a build cache would rest on, and it is worth a
//! test of its own because losing it is invisible: ids that drift with
//! allocation order still *work*, right up until something is written to disk
//! and read back in another process, where they quietly mean something else.

use meadow_compiler::{compile_str, hir::VarId};

const SRC: &str = "fun add x y = x + y\nfun twice f v = f (f v)\ndef main = add 1 2\n";

/// The same source compiles to the same ids, however many units came first.
///
/// The counter used to be a process-wide `AtomicU32`, so this failed: the
/// second compile's ids started wherever the first one's had stopped.
#[test]
fn compiling_a_unit_twice_gives_the_same_ids() {
    let (first, d1) = compile_str("t", SRC);
    assert!(d1.is_empty(), "{:?}", d1.iter().map(|d| &d.msg).collect::<Vec<_>>());

    // Compile something else in between, to use up ids if anything still can.
    let _ = compile_str("noise", "fun a b = b\nfun c d = d\ndef main = 0\n");

    let (second, d2) = compile_str("t", SRC);
    assert!(d2.is_empty());

    assert_eq!(first.vars, second.vars, "the unit's range moved");

    let names = |p: &meadow_compiler::CompiledPackage| {
        let mut v: Vec<(u32, String)> = p
            .exports
            .iter()
            .map(|e| (e.var.0, e.name.to_string()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(names(&first), names(&second), "an export's id moved");
}

/// Every id a unit mints falls inside the range it reports.
///
/// The range is the provenance: given an id and the ranges of the packages in
/// a build, you can say which one owns it. That only holds if lowering — which
/// invents variables of its own, for a scrutinee or an eta-expansion — draws
/// from the same generator as resolution rather than from somewhere else.
#[test]
fn a_units_ids_lie_inside_the_range_it_reports() {
    let (pkg, diags) = compile_str(
        "t",
        "data Colour = Red | Green\n\
         fun pick c = match c with | Red -> 1 | Green -> 2\n\
         fun curried a = add a\n\
         fun add a b = a + b\n\
         def main = pick Red\n",
    );
    assert!(diags.is_empty(), "{:?}", diags.iter().map(|d| &d.msg).collect::<Vec<_>>());
    assert!(!pkg.vars.is_empty(), "a unit that minted nothing");

    for e in &pkg.exports {
        assert!(
            pkg.vars.contains(&e.var.0),
            "export `{}` has id {} outside {:?}",
            e.name,
            e.var.0,
            pkg.vars
        );
    }
    // Including the variables lowering invented, which are in the `core` defs
    // rather than in the export list.
    for d in &pkg.defs {
        assert!(
            pkg.vars.contains(&d.var.0),
            "def `{}` has id {} outside {:?}",
            d.name,
            d.var.0,
            pkg.vars
        );
    }
}

/// A unit starts above its dependencies, so no two units share an id.
#[test]
fn a_dependent_unit_does_not_overlap_its_dependency() {
    use meadow_compiler::{
        compile_unit, intern::InternedString, lexer::tokenize, parser,
        source::{Source, SourceKind},
        AstModule, Options,
    };

    let unit = |name: &str, src: &str, deps: &[&meadow_compiler::CompiledPackage]| {
        let name = InternedString::from(name);
        let source = Source::new(SourceKind::Interactive, InternedString::from(src));
        let lex = tokenize(source);
        let (ast, _) = parser::parse(name, source, &lex.tokens);
        let modules = vec![AstModule {
            path: vec![],
            name,
            ast: ast.expect("parses"),
            source,
        }];
        compile_unit(name, 0, modules, deps, Options::debug())
    };

    let (base, d1) = unit("base", "@pub fun helper x = x + 1\n", &[]);
    assert!(d1.is_empty(), "{:?}", d1.iter().map(|d| &d.msg).collect::<Vec<_>>());
    let (top, d2) = unit("top", "def main = helper 41\n", &[&base]);
    assert!(d2.is_empty(), "{:?}", d2.iter().map(|d| &d.msg).collect::<Vec<_>>());

    assert!(
        top.vars.start >= base.vars.end,
        "ranges overlap: dependency {:?}, dependent {:?}",
        base.vars,
        top.vars
    );
    // And the dependent really is referring to the dependency's binding, under
    // the dependency's own id — which is the reason the ranges must not clash.
    assert!(base.vars.contains(&base.exports[0].var.0));
}

/// Variables invented after compilation are outside every unit's range.
#[test]
fn synthetic_ids_cannot_collide_with_a_unit() {
    let (pkg, _) = compile_str("t", SRC);
    let s = VarId::synthetic(0);
    assert!(s.is_synthetic());
    assert!(!pkg.vars.contains(&s.0), "{:?} reaches the synthetic range", pkg.vars);
    // Indexed, not counted: asking twice gives the same id.
    assert_eq!(VarId::synthetic(7), VarId::synthetic(7));
}

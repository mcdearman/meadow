//! Pattern synonyms, `pattern P x y = p` / `pattern P x y <- p`: a name for a
//! pattern, which matches -- and, where it can, builds -- as `p` does. What
//! they mean on every machine, and what the checker says of them.

mod common;
use meadow::{Engine, OptLevel, Options, pipeline, runtime};

/// The CEK machine's answer, required of the VM and its JIT at every level
/// and of a release build.
fn agreed(src: &str) -> String {
    let (program, diags) = pipeline::compile_str_with_std("test", src, Options::debug());
    assert!(
        diags.is_empty(),
        "compile errors in\n{src}\n{}",
        diags
            .iter()
            .map(|d| d.msg.clone())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let program = meadow_compiler::core::prune::prune(&program);
    let cek = runtime::run(&program, Engine::Cek, OptLevel::O1)
        .unwrap_or_else(|e| panic!("the CEK machine failed on\n{src}\n{e}"));
    for engine in [Engine::Vm, Engine::Jit] {
        for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2] {
            let got = runtime::run(&program, engine, opt)
                .unwrap_or_else(|e| panic!("{engine:?} at {} failed: {e}", opt.name()));
            assert_eq!(got, cek, "{engine:?} at {} on\n{src}", opt.name());
        }
    }
    let (release, diags) = pipeline::compile_str_with_std("test", src, Options::release());
    assert!(
        diags.is_empty(),
        "release: {:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let release = meadow_compiler::core::prune::prune(&release);
    let got = runtime::run(&release, Engine::Jit, OptLevel::O2).unwrap();
    assert_eq!(got, cek, "a release build on\n{src}");
    cek
}

fn is(src: &str, expected: &str) {
    assert_eq!(agreed(src), expected, "{src}");
}

fn errors(src: &str) -> String {
    common::errors_std_with(src, Options::debug())
}

#[test]
fn a_synonym_matches_and_builds_as_its_pattern_does() {
    is(
        "pattern Pair x y = (x, y)\n\
         pattern Head x <- x :: _\n\
         fun first xs = match xs with\n\
         | Head x -> x\n\
         | _ -> 0\n\
         fun swap p = match p with\n\
         | Pair a b -> Pair b a\n\
         def main = (first [3; 4], first [;], swap (Pair 1 2))\n",
        "(3, 0, (2, 1))",
    );
}

#[test]
fn a_synonym_takes_any_number_of_arguments() {
    is(
        "pattern Origin = (0, 0)\n\
         pattern Two x = (x, 2)\n\
         pattern Triple a b c = (a, (b, c))\n\
         fun f p = match p with\n\
         | Origin -> \"origin\"\n\
         | Two _ -> \"two\"\n\
         | _ -> \"elsewhere\"\n\
         fun g t = match t with | Triple a b c -> a + b + c\n\
         def main = (f Origin, f (Two 4), f (1, 3), g (Triple 1 2 3))\n",
        r#"("origin", "two", "elsewhere", 6)"#,
    );
}

/// A synonym's pattern can hold a view, and a synonym that only matches can
/// say how it builds after a `where`: a data type seen as another.
#[test]
fn a_synonym_can_look_through_a_view() {
    is(
        "data Nat = Nat Int\n\
         use Nat.*\n\
         fun pred (Nat n) = if n > 0 then Just (Nat (n - 1)) else None\n\
         pattern Zero <- (pred -> None) where Zero = Nat 0\n\
         pattern Succ m <- (pred -> Just m) where Succ (Nat n) = Nat (n + 1)\n\
         fun toInt n = match n with\n\
         | Zero -> 0\n\
         | Succ m -> 1 + toInt m\n\
         | _ -> 0 - 1\n\
         fun add a b = match a with\n\
         | Succ m -> Succ (add m b)\n\
         | _ -> b\n\
         def main = toInt (add (Succ (Succ Zero)) (Succ Zero))\n",
        "3",
    );
}

#[test]
fn a_synonym_is_a_parameter_and_nests() {
    is(
        "pattern Pair x y = (x, y)\n\
         pattern Box x = [x;]\n\
         fun sum (Pair a b) = a + b\n\
         fun unbox xs = match xs with\n\
         | Box (Pair a (Box b)) -> a + b\n\
         | _ -> 0\n\
         def main = (sum (Pair 1 2), unbox (Box (Pair 10 (Box 5))), unbox [;])\n",
        "(3, 15, 0)",
    );
}

#[test]
fn a_synonym_is_exported_and_imported_by_name() {
    // Without a library, too: a synonym is made of the compiler's own lists.
    let out = common::eval_unit(&[
        (
            "",
            "mod Shapes\n\
             use Shapes (Square, Shape)\n\
             use Shapes.Shape.*\n\
             def main = match Square 3 with | Square s -> s * 10 | Rect w _ -> w | Dot -> 0\n",
        ),
        (
            "Shapes",
            "use Shape.*\n\
             @pub data Shape = Rect Int Int | Dot\n\
             fun side r = match r with | Rect w h -> if w == h then [w;] else [;] | Dot -> [;]\n\
             @pub pattern Square s <- (side -> [s;]) where Square s = Rect s s\n",
        ),
    ]);
    assert_eq!(out, "30");
}

#[test]
fn a_synonym_says_what_is_wrong_with_it() {
    // A parameter its pattern does not bind, and a name it binds that is not
    // a parameter.
    let e = errors("pattern P x y = (x, x)\ndef main = 0\n");
    assert!(
        e.contains("does not bind") || e.contains("more than once"),
        "{e}"
    );
    let e = errors("pattern P x = (x, y)\ndef main = 0\n");
    assert!(e.contains("not one of its parameters"), "{e}");
    // One that only matches cannot build.
    let e = errors("pattern Head x <- x :: _\ndef main = Head 1\n");
    assert!(e.contains("only matches"), "{e}");
    // A pattern with a view cannot be read as a value.
    let e = errors("pattern P x = (id -> x)\ndef main = 0\n");
    assert!(e.contains("cannot be built"), "{e}");
    // Used with the wrong number of arguments.
    let e = errors(
        "pattern Pair x y = (x, y)\n\
         fun f p = match p with | Pair a -> a\n\
         def main = f (1, 2)\n",
    );
    assert!(e.contains("takes 2 arguments, not 1"), "{e}");
}

#[test]
fn pattern_is_still_a_name() {
    is(
        "fun pattern x = x + 1\ndef main = let pattern = 2 in pattern * 3\n",
        "6",
    );
}

const NAT: &str = "data Nat = Nat Int\n\
     use Nat.*\n\
     fun pred (Nat n) = if n > 0 then Just (Nat (n - 1)) else None\n\
     pattern Zero <- (pred -> None) where Zero = Nat 0\n\
     pattern Succ m <- (pred -> Just m) where Succ (Nat n) = Nat (n + 1)\n";

/// `pattern A | B` says the synonyms cover their type together: a `match`
/// with an arm for each, matching whatever it binds, needs nothing more.
#[test]
fn a_set_of_synonyms_covers_its_type() {
    let src = format!(
        "{NAT}pattern Zero | Succ\n\
         fun toInt n = match n with\n\
         | Zero -> 0\n\
         | Succ m -> 1 + toInt m\n\
         def main = toInt (Succ (Succ Zero))\n"
    );
    assert_eq!(common::errors_std_with(&src, Options::release()), "");
    assert_eq!(agreed(&src), "2");
    // Without the set, the views cover nothing.
    let without = src.replace("pattern Zero | Succ\n", "");
    let e = common::errors_std_with(&without, Options::release());
    assert!(e.contains("non-exhaustive"), "{e}");
}

#[test]
fn a_set_says_which_of_it_is_missing() {
    let e = common::errors_std_with(
        &format!(
            "{NAT}pattern Zero | Succ\n\
             fun f n = match n with | Succ m -> 1\n\
             def main = f Zero\n"
        ),
        Options::release(),
    );
    assert!(e.contains("`Zero` is not matched"), "{e}");
    // An argument that does not match everything covers only what it does --
    // and is split by the set, being one of it.
    let e = common::errors_std_with(
        &format!(
            "{NAT}pattern Zero | Succ\n\
             fun f n = match n with | Zero -> 0 | Succ Zero -> 1\n\
             def main = f Zero\n"
        ),
        Options::release(),
    );
    assert!(e.contains("`Succ (Succ _)` is not matched"), "{e}");
    let e = errors(&format!("{NAT}pattern Zero | Nope\ndef main = 0\n"));
    assert!(e.contains("`Nope` is not a pattern synonym"), "{e}");
}

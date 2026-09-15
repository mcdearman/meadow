//! `type` aliases, standalone signatures and record update: what they mean on
//! every machine, and what the checker says when they are wrong.

mod common;
use meadow::{Engine, OptLevel, Options, pipeline, runtime};
use std::path::{Path, PathBuf};

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

// --- type aliases -------------------------------------------------------------

#[test]
fn an_alias_means_what_it_stands_for() {
    is(
        "type Point = (Int, Int)\n\
         type Pair a = (a, a)\n\
         type Grid = [Pair Point;]\n\
         fun add ((a, b) : Point) ((c, d) : Point) : Point = (a + c, b + d)\n\
         def grid : Grid = [((0, 0), (1, 1)); ((2, 3), (4, 5))]\n\
         def main = (add (1, 2) (10, 20), grid)\n",
        "((11, 22), [((0, 0), (1, 1)); ((2, 3), (4, 5))])",
    );
    // In a declaration, and with an effect in it.
    is(
        "type Name = String\n\
         type Action a = a -> () ! Console\n\
         data Named = Named Name Int\n\
         record Greeter = { who : Name, act : Action Name }\n\
         def main = let g = Greeter { who = \"Ann\", act = \\n -> println n } in\n\
           let _ = g.act g.who in\n\
           match Named.Named g.who 3 with | Named.Named n k -> (n, k)\n",
        r#"("Ann", 3)"#,
    );
}

#[test]
fn an_alias_is_not_a_new_type() {
    // A `Meters` is an `Int` wherever one is wanted, and the other way round.
    let schemes = common::schemes_std(
        "type Meters = Int\nfun twice (m : Meters) = m * 2\ndef x = twice 21 + 1\n",
    );
    assert!(schemes.contains("twice : Int -> Int"), "{schemes}");
    assert!(!schemes.contains("!!"), "{schemes}");
}

#[test]
fn an_alias_cannot_refer_to_itself() {
    let out = errors("type A = [B]\ntype B = (A, Int)\ndef main = 1\n");
    assert!(out.contains("type alias `A` refers to itself"), "{out}");
    let out = errors("type T a = [a]\ndef x : T = []\ndef main = 1\n");
    assert!(out.contains("type `T` takes 1 argument(s), got 0"), "{out}");
}

#[test]
fn an_alias_is_as_visible_as_it_says() {
    let ok = common::eval_unit(&[
        (
            "Geo",
            "@pub(pkg) type Point = (Int, Int)\nfun origin : Point\n@pub(pkg) fun origin = (0, 0)\n",
        ),
        (
            "",
            "use Geo (Point, origin)\nfun shift ((x, y) : Point) : Point = (x + 1, y)\ndef main = shift origin\n",
        ),
    ]);
    assert_eq!(ok, "(1, 0)");
    let out = common::unit_errors(&[
        (
            "Geo",
            "type Point = (Int, Int)\n@pub(pkg) def origin = (0, 0)\n",
        ),
        ("", "use Geo (Point)\ndef main = 1\n"),
    ]);
    assert!(out.contains("`Point` is private to module `Geo`"), "{out}");
}

fn scratch(who: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-decl-{}-{who}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A dependent sees a package's `@pub` alias, and not one it kept.
#[test]
fn a_pub_alias_crosses_the_package() {
    let root = scratch("pkg");
    write(
        &root.join("geo/meadow.toml"),
        "[package]\nname = \"geo\"\nversion = \"0.1.0\"\n",
    );
    write(
        &root.join("geo/src/Lib.mw"),
        "@pub type Point = (Int, Int)\ntype Secret = Int\n\
         fun norm : Point -> Int\n@pub fun norm (x, y) = x * x + y * y\n",
    );
    let app = |main: &str| {
        write(
            &root.join("app/meadow.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ngeo = { path = \"../geo\" }\n",
        );
        write(&root.join("app/src/Main.mw"), main);
        pipeline::build(&root.join("app"), Options::debug())
    };
    let out = app("use geo (norm)\ndef p : Point = (3, 4)\ndef main = norm p\n");
    assert!(
        out.diagnostics.is_empty(),
        "{:?}",
        out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let program = out.linked.unwrap().program;
    assert_eq!(meadow_eval::run(&program).unwrap().to_string(), "25");

    let out = app("def s : Secret = 1\ndef main = s\n");
    let msgs: Vec<_> = out.diagnostics.iter().map(|d| d.msg.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("unknown type `Secret`")),
        "{msgs:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

// --- standalone signatures ----------------------------------------------------

#[test]
fn a_signature_gives_a_binding_its_type() {
    is(
        "fun swap : (a, b) -> (b, a)\n\
         fun swap (x, y) = (y, x)\n\
         fun apply : (a -> b ! e) -> a -> b ! e\n\
         fun apply f x = f x\n\
         def small : Int8\n\
         def small = 5\n\
         fun names : () -> [String;]\n\
         fun names () = [\"a\"; \"b\"]\n\
         def main = let _ = apply println \"hi\" in (swap (1, \"x\"), swap (True, 2), small, names ())\n",
        r#"(("x", 1), (2, True), 5, ["a"; "b"])"#,
    );
    // After its definition too, and for mutually recursive ones.
    is(
        "fun isEven n = if n == 0 then True else isOdd (n - 1)\n\
         fun isOdd n = if n == 0 then False else isEven (n - 1)\n\
         fun isEven : Int -> Bool\n\
         fun isOdd : Int -> Bool\n\
         def main = (isEven 10, isOdd 7)\n",
        "(True, True)",
    );
    let schemes = common::schemes_std("fun f : Int32 -> Int32\nfun f x = x + 1\n");
    assert!(schemes.contains("f : Int32 -> Int32"), "{schemes}");
}

#[test]
fn a_definition_must_be_as_general_as_its_signature() {
    let out = errors("fun f : a -> a\nfun f x = x ++ \"s\"\ndef main = f \"1\"\n");
    assert!(
        out.contains("less general than its signature: the signature says `a -> a`, and the definition is `String -> String`"),
        "{out}"
    );
    let out = errors("fun f : a -> a\nfun f x = x + 1\ndef main = f 1\n");
    assert!(out.contains("needs a number where its signature"), "{out}");
    let out = errors("fun f : a -> b -> a\nfun f x y = if True then x else y\ndef main = f 1 2\n");
    assert!(out.contains("less general than its signature"), "{out}");
    // An effect the signature does not allow.
    let out = errors("fun f : Int -> Int\nfun f x = let _ = println x in x\ndef main = f 1\n");
    assert!(
        out.contains("the effect `Console` is not allowed here"),
        "{out}"
    );
}

#[test]
fn a_signature_belongs_to_one_definition() {
    let out = errors("fun g : Int -> Int\ndef main = 1\n");
    assert!(
        out.contains("a signature for `g`, which is not defined in this module"),
        "{out}"
    );
    let out = errors("fun f : Int -> Int\nfun f : Int -> Int\nfun f x = x\ndef main = f 1\n");
    assert!(out.contains("`f` already has a signature"), "{out}");
    let out = errors("@pub fun f : Int -> Int\nfun f x = x\ndef main = f 1\n");
    assert!(
        out.contains("a signature cannot carry `@pub` or `@test`"),
        "{out}"
    );
}

// --- record update --------------------------------------------------------------

#[test]
fn an_update_replaces_the_fields_it_names() {
    is(
        "record Person = { name : String, age : Int, tags : [String;] }\n\
         fun birthday (p : Person) = { p | age = p.age + 1 }\n\
         def ann = Person { name = \"Ann\", age = 41, tags = [\"a\";] }\n\
         def main = let older = birthday ann in\n\
           let renamed = { older | tags = [;], name = \"Bo\" } in\n\
           (ann.age, older.age, older.name, renamed.name, renamed.age, renamed.tags, older.tags)\n",
        r#"(41, 42, "Ann", "Bo", 42, [], ["a"])"#,
    );
    // A record type with parameters, with unboxed fields beside boxed ones.
    is(
        "record Box a = { item : a, count : Int, weight : Float }\n\
         fun refill (b : Box a) (x : a) : Box a = { b | item = x, count = b.count + 1 }\n\
         def main = let b = refill (Box { item = [1; 2], count = 1, weight = 2.5 }) [3;] in\n\
           (b.item, b.count, b.weight)\n",
        "([3], 2, 2.5)",
    );
    // An anonymous record, including through a function that knows only one of
    // its fields.
    is(
        "fun reset r = { r | count = 0 }\n\
         def main = let r = { count = 5, name = \"x\" } in\n\
           let s = reset r in\n\
           let t = { s | name = \"y\", count = s.count + 2 } in\n\
           (r.count, s.count, s.name, t.count, t.name)\n",
        r#"(5, 0, "x", 2, "y")"#,
    );
    // Values are evaluated in the order they are written, and the record once.
    is(
        "use Std.St as St\n\
         record P = { a : Int, b : Int }\n\
         def main = runSt (\\() ->\n\
           let log = St.newRef 0 in\n\
           let note tag = St.modifyRef log (\\n -> n * 10 + tag) in\n\
           let p = { (let _ = note 1 in P { a = 0, b = 0 }) | b = (let _ = note 2 in 2), a = (let _ = note 3 in 1) } in\n\
           (p.a, p.b, St.getRef log))\n",
        "(1, 2, 123)",
    );
}

#[test]
fn an_update_keeps_every_field_and_its_type() {
    let out = errors(
        "record P = { x : Int }\nfun f (p : P) = { p | y = 1 }\ndef main = f (P { x = 1 })\n",
    );
    assert!(out.contains("`P` has no field `y`"), "{out}");
    let out = errors(
        "record P = { x : Int }\nfun f (p : P) = { p | x = \"s\" }\ndef main = f (P { x = 1 })\n",
    );
    assert!(out.contains("type mismatch: `Int` vs `String`"), "{out}");
    let out = errors(
        "data S = A { x : Int } | B { x : Int }\nfun f (s : S) = { s | x = 1 }\ndef main = 1\n",
    );
    assert_eq!(
        out,
        "`S` has more than one constructor, so there is no one record to update"
    );
    let out = errors("def r = { x = 1 }\ndef main = { r | x = 2, x = 3 }\n");
    assert_eq!(out, "field `x` is updated twice");
    let out = errors("def main = { { x = 1 } | y = 2 }\n");
    assert!(out.contains("record has no field `y`"), "{out}");
}

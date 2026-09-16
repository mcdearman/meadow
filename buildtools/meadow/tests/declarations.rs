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

// --- record types ----------------------------------------------------------------

/// `{ name : String | r }` is written the way a hover prints it, and means the
/// row a record literal already has -- in a signature, an annotation, a result
/// type or an alias.
#[test]
fn a_record_type_can_be_written_wherever_a_type_can() {
    is(
        "fun getName : { name : String | r } -> String = \\p -> p.name\n\
         fun byParam (p : { name : String | r }) : String = p.name\n\
         fun mk (s : String) : { name : String } = { name = s }\n\
         type Person = { name : String, home : { city : String } }\n\
         fun city (p : Person) : String = p.home.city\n\
         def main = (getName { name = \"ada\", age = 36 }, byParam { name = \"bo\" }, \
                     (mk \"cy\").name, city { name = \"di\", home = { city = \"Rome\" } })\n",
        r#"("ada", "bo", "cy", "Rome")"#,
    );
    // The same as a standalone signature, and inside another type.
    is(
        "fun getName : { name : String | r } -> String\n\
         fun getName p = p.name\n\
         fun first : Maybe { name : String | r } -> String\n\
         fun first m = match m with | Just p -> p.name | None -> \"\"\n\
         def main = (getName { name = \"x\", y = 1 }, first (Just { name = \"q\", z = 2 }))\n",
        r#"("x", "q")"#,
    );
}

/// The row variable is one variable: what comes in with the named field keeps
/// every other field it had, and the result says so.
#[test]
fn a_row_variable_carries_the_fields_it_stands_for() {
    is(
        "fun keep : { name : String | r } -> { name : String | r }\n\
         fun keep p = p\n\
         def main = (keep { name = \"d\", age = 4 }).age\n",
        "4",
    );
    let out = errors(
        "fun keep : { name : String | r } -> { name : String | r }\n\
         fun keep p = { name = p.name }\n\
         def main = 0\n",
    );
    assert!(out.contains("less general than its signature"), "{out}");
}

/// Closed means closed, open still needs the fields it names, and a row cannot
/// say one label twice.
#[test]
fn a_record_type_is_checked_like_one() {
    let out = errors(
        "fun f : { name : String } -> String = \\p -> p.name\ndef main = f { name = \"a\", age = 1 }\n",
    );
    assert!(out.contains("no field `age`"), "{out}");
    let out = errors(
        "fun f : { name : String | r } -> String = \\p -> p.name\ndef main = f { age = 1 }\n",
    );
    assert!(out.contains("no field `name`"), "{out}");
    let out = errors("fun f (p : { x : Int, x : Bool }) : Int = 1\ndef main = 0\n");
    assert!(out.contains("field `x` appears twice"), "{out}");
    is("fun unit (u : {}) : Int = 1\ndef main = unit {}\n", "1");
}

/// A variant's own braces are its named fields; a positional field of record
/// type is parenthesised, and both still mean what they did.
#[test]
fn a_variants_braces_are_still_its_fields() {
    is(
        "data Shape = Rect { w : Int, h : Int }\n\
         data Box = Box ({ w : Int })\n\
         fun area (s : Shape) : Int = match s with | Shape.Rect { w, h } -> w * h\n\
         fun width (b : Box) : Int = match b with | Box.Box r -> r.w\n\
         def main = (area (Shape.Rect { w = 2, h = 3 }), width (Box.Box { w = 7 }))\n",
        "(6, 7)",
    );
}

/// `Shape.Rect { w = 2, h = 3 }` names the fields as the unqualified spelling
/// does, building and matching alike. The qualified path used to resolve the
/// constructor and then take the braces as one positional record argument --
/// and qualified is how a constructor is written unless its type is `use`d.
#[test]
fn a_qualified_constructor_takes_named_fields() {
    is(
        "data Shape = Rect { w : Int, h : Int }\n\
         fun area (s : Shape) : Int = match s with | Shape.Rect { h, w } -> w - h\n\
         def main = (area (Shape.Rect { h = 3, w = 10 }), area (Shape.Rect { w = 5, h = 1 }))\n",
        "(7, 4)",
    );
    let out = errors(
        "data Shape = Rect { w : Int, h : Int }\ndef main = Shape.Rect { w = 2, depth = 3 }\n",
    );
    assert!(out.contains("no field `depth`"), "{out}");
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

// --- functions written as several equations ----------------------------------

#[test]
fn a_function_may_be_written_as_several_equations() {
    is(
        r#"
fun gcd a 0 = a
  | gcd a b = gcd b (a % b)

def main = gcd 48 18
"#,
        "6",
    );
}

#[test]
fn equations_are_tried_in_the_order_they_are_written() {
    is(
        r#"
fun describe 0 = "zero"
  | describe 1 = "one"
  | describe n = "many"

def main = (describe 0, describe 1, describe 7)
"#,
        r#"("zero", "one", "many")"#,
    );
}

#[test]
fn an_equation_may_match_a_constructor_that_takes_nothing() {
    is(
        r#"
use Std.Maybe.Maybe.*

fun orElse d None     = d
  | orElse _ (Just x) = x

def main = (orElse 9 None, orElse 9 (Just 1))
"#,
        "(9, 1)",
    );
}

#[test]
fn equations_may_match_lists() {
    is(
        r#"
fun len [;]       = 0
  | len (_ :: xs) = 1 + len xs

def main = len [1; 2; 3]
"#,
        "3",
    );
}

#[test]
fn a_match_in_the_body_is_not_read_as_more_equations() {
    // The arms of a `match` begin with `|` too. They are told apart by shape:
    // an equation is a name, patterns and `=`, and an arm is a pattern and `->`.
    is(
        r#"
fun classify n = match compare n 0 with
  | Less    -> "neg"
  | Equal   -> "zero"
  | Greater -> "pos"

def main = (classify (-5), classify 0, classify 5)
"#,
        r#"("neg", "zero", "pos")"#,
    );
}

#[test]
fn several_equations_can_be_recursive_and_polymorphic() {
    is(
        r#"
fun count [;]       = 0
  | count (_ :: xs) = 1 + count xs

def main = (count [1; 2], count ["a"; "b"; "c"])
"#,
        "(2, 3)",
    );
}

#[test]
fn a_declared_type_still_applies_to_every_equation() {
    is(
        r#"
fun gcd a b : Int = gcd' a b
fun gcd' a 0 = a
  | gcd' a b = gcd' b (a % b)

def main = gcd 48 18
"#,
        "6",
    );
}

#[test]
fn one_equation_that_names_something_else_is_reported() {
    let e = errors(
        r#"
fun f a 0 = a
  | g a b = b

def main = f 1 0
"#,
    );
    assert!(
        e.contains("this equation defines `g`, but the ones above it define `f`"),
        "{e}"
    );
}

#[test]
fn equations_that_take_different_numbers_of_arguments_are_reported() {
    let e = errors(
        r#"
fun f a 0 = a
  | f a = a

def main = f 1 0
"#,
    );
    assert!(
        e.contains("takes 1 argument, but the first takes 2"),
        "{e}"
    );
}

#[test]
fn equations_with_nothing_to_match_on_are_reported() {
    let e = errors(
        r#"
fun f = 1
  | f = 2

def main = f
"#,
    );
    assert!(e.contains("takes no arguments"), "{e}");
}

#[test]
fn equations_that_do_not_cover_everything_are_reported() {
    // The sugar is a `match`, so it is checked like one.
    let e = common::errors_std_with(
        r#"
fun describe 0 = "zero"
  | describe 1 = "one"

def main = describe 0
"#,
        Options::release(),
    );
    assert!(e.contains("non-exhaustive"), "{e}");
}

#[test]
fn a_single_equation_may_still_match_a_literal() {
    // It is then refutable, which is the irrefutability check's to report --
    // a better error than the parse failure this used to be.
    let e = common::errors_std_with(
        r#"
fun f 0 = "zero"
def main = f 0
"#,
        Options::release(),
    );
    assert!(e.contains("refutable pattern in function parameter"), "{e}");
}

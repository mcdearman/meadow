//! Traits: what they mean on every machine, what inference works out for a
//! function nobody annotated, and what the checker says when one is misused.

mod common;
use meadow::{Engine, OptLevel, Options, pipeline, runtime};
use std::path::{Path, PathBuf};

/// The CEK machine's answer, required of the VM and its JIT at every level
/// and of a release build, where dictionaries meet the specializer.
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

const DESCRIBE: &str = "trait Describe a {\n\
    \x20 fun describe : a -> String\n\
    \x20 fun twice : a -> String\n\
    \x20 fun twice x = describe x ++ describe x\n\
    }\n\
    impl Describe Int { fun describe n = \"int \" ++ show n }\n\
    impl Describe Bool {\n\
    \x20 fun describe b = if b then \"yes\" else \"no\"\n\
    \x20 fun twice b = \"bool!\"\n\
    }\n\
    impl Describe [a;] where Describe a {\n\
    \x20 fun describe xs = match xs with\n\
    \x20   | [;] -> \".\"\n\
    \x20   | x :: rest -> describe x ++ \",\" ++ describe rest\n\
    }\n\
    impl Describe (a, b) where Describe a, Describe b {\n\
    \x20 fun describe p = match p with | (x, y) -> describe x ++ \"+\" ++ describe y\n\
    }\n";

#[test]
fn a_method_is_the_impl_of_the_type_it_meets() {
    is(
        &format!(
            "{DESCRIBE}def main = (describe 3, describe True, describe [1; 2], describe (1, [True;]))\n"
        ),
        r#"("int 3", "yes", "int 1,int 2,.", "int 1+yes,.")"#,
    );
    // A default is what an `impl` that leaves the method out gets, and one
    // that defines it does not.
    is(
        &format!("{DESCRIBE}def main = (twice 4, twice False, twice [7;])\n"),
        r#"("int 4int 4", "bool!", "int 7,.int 7,.")"#,
    );
}

#[test]
fn a_function_asks_for_what_its_body_needs() {
    // Nobody wrote a signature: the `where` is inferred, and printed.
    let schemes = common::schemes_std(&format!(
        "{DESCRIBE}fun both x y = describe x ++ \" & \" ++ describe y\n\
         fun loud : a -> String where Describe a\n\
         fun loud x = twice x ++ \"!\"\n"
    ));
    assert!(
        schemes.contains("both : forall a b. a -> b -> String where Describe a, Describe b"),
        "{schemes}"
    );
    assert!(
        schemes.contains("loud : forall a. a -> String where Describe a"),
        "{schemes}"
    );
    is(
        &format!(
            "{DESCRIBE}fun both x y = describe x ++ \" & \" ++ describe y\n\
             fun loud : a -> String where Describe a\n\
             fun loud x = twice x ++ \"!\"\n\
             fun nested : a -> String where Describe a\n\
             fun nested x = describe [(x, x); (x, x)]\n\
             def main = (both True [False;], loud [True;], nested 1)\n"
        ),
        r#"("yes & no,.", "yes,.yes,.!", "int 1+int 1,int 1+int 1,.")"#,
    );
}

/// Functions that call one another share what they ask for, and pass it on.
#[test]
fn mutual_recursion_passes_its_dictionaries_along() {
    is(
        &format!(
            "{DESCRIBE}fun evens n x = if n == 0 then describe x else odds (n - 1) x\n\
             fun odds n x = if n == 0 then \"odd \" ++ describe x else evens (n - 1) x\n\
             fun count n x = if n == 0 then describe x else count (n - 1) x\n\
             def main = (evens 4 True, odds 3 [1;], evens 3 2, count 5 (True, 1))\n"
        ),
        r#"("yes", "int 1,.", "odd int 2", "yes+int 1")"#,
    );
}

#[test]
fn a_trait_can_require_another() {
    let src = "trait Same a { fun same : a -> a -> Bool }\n\
        trait Ranked a where Same a { fun before : a -> a -> Bool }\n\
        impl Same Int { fun same x y = x == y }\n\
        impl Ranked Int { fun before x y = x < y }\n\
        impl Same [a;] where Same a {\n\
        \x20 fun same xs ys = match (xs, ys) with\n\
        \x20   | ([;], [;]) -> True\n\
        \x20   | (x :: a, y :: b) -> same x y and same a b\n\
        \x20   | _ -> False\n\
        }\n\
        impl Ranked [a;] where Ranked a {\n\
        \x20 fun before xs ys = match (xs, ys) with\n\
        \x20   | (_, [;]) -> False\n\
        \x20   | ([;], _) -> True\n\
        \x20   | (x :: a, y :: b) -> before x y or (same x y and before a b)\n\
        }\n\
        fun atMost : a -> a -> Bool where Ranked a\n\
        fun atMost x y = before x y or same x y\n\
        def main = (atMost 1 1, atMost 2 1, atMost [1; 2] [1; 3], atMost [2;] [1; 9])\n";
    is(src, "(True, False, True, False)");
    // An `impl` of the one needs an `impl` of the other.
    let out = errors(
        "trait Same a { fun same : a -> a -> Bool }\n\
         trait Ranked a where Same a { fun before : a -> a -> Bool }\n\
         impl Ranked Int { fun before x y = x < y }\n\
         def main = before 1 2\n",
    );
    assert!(out.contains("`Int` does not implement `Same`"), "{out}");
}

#[test]
fn an_associated_type_is_the_impls_to_choose() {
    let src = "trait Container f {\n\
        \x20 type Elem f\n\
        \x20 fun empty : () -> f\n\
        \x20 fun insert : Elem f -> f -> f\n\
        \x20 fun toList : f -> [Elem f;]\n\
        }\n\
        record Bag a = { items : [a;] }\n\
        record Bits = { word : Int }\n\
        impl Container (Bag a) {\n\
        \x20 type Elem (Bag a) = a\n\
        \x20 fun empty u = Bag { items = [;] }\n\
        \x20 fun insert x b = Bag { items = x :: b.items }\n\
        \x20 fun toList b = b.items\n\
        }\n\
        impl Container Bits {\n\
        \x20 type Elem Bits = Bool\n\
        \x20 fun empty u = Bits { word = 1 }\n\
        \x20 fun insert x b = Bits { word = b.word * 2 + (if x then 1 else 0) }\n\
        \x20 fun toList b = if b.word <= 1 then [;] else (b.word % 2 == 1) :: toList (Bits { word = b.word / 2 })\n\
        }\n\
        fun fromList : [Elem f;] -> f where Container f\n\
        fun fromList xs = match xs with\n\
        \x20 | [;] -> empty ()\n\
        \x20 | x :: rest -> insert x (fromList rest)\n\
        fun bag : [a;] -> Bag a\n\
        fun bag xs = fromList xs\n\
        fun bits : [Bool;] -> Bits\n\
        fun bits xs = fromList xs\n\
        def main = (toList (bag [\"a\"; \"b\"]), (bits [True; False; True]).word, toList (bits [False; True]))\n";
    is(src, r#"(["a"; "b"], 13, [False; True])"#);
    // What it is comes with the `impl`: a `Bits` holds `Bool`s and nothing else.
    let out = errors(&format!(
        "{}def wrong = insert 3 (bits [True;])\n",
        src.replace("def main", "def unused")
    ));
    assert!(out.contains("type mismatch"), "{out}");
}

#[test]
fn a_method_may_be_general_in_its_effects() {
    is(
        "trait Each f {\n\
         \x20 type Item f\n\
         \x20 fun each : (Item f -> () ! e) -> f -> () ! e\n\
         }\n\
         impl Each [a;] {\n\
         \x20 type Item [a;] = a\n\
         \x20 fun each f xs = match xs with\n\
         \x20   | [;] -> ()\n\
         \x20   | x :: rest -> let _ = f x in each f rest\n\
         }\n\
         use Std.St as St\n\
         fun total xs = runSt (\\() ->\n\
         \x20 let sum = St.newRef 0 in\n\
         \x20 let _ = each (\\x -> St.modifyRef sum (\\s -> s + x)) xs in\n\
         \x20 St.getRef sum)\n\
         def main = total [1; 2; 3; 4]\n",
        "10",
    );
}

#[test]
fn what_is_wrong_is_said() {
    let with = |rest: &str| errors(&format!("{DESCRIBE}{rest}"));
    let out = with("def main = describe \"x\"\n");
    assert!(
        out.contains("`String` does not implement `Describe`"),
        "{out}"
    );
    let out = with("fun f : a -> String\nfun f x = describe x\ndef main = f 1\n");
    assert!(
        out.contains("this needs `Describe a`, which the signature does not ask for"),
        "{out}"
    );
    let out = with("impl Describe Int { fun describe n = \"again\" }\ndef main = 1\n");
    assert!(
        out.contains("`Describe` is already implemented for `Int`"),
        "{out}"
    );
    let out = with("impl Describe String { }\ndef main = 1\n");
    assert!(
        out.contains("this `impl Describe` is missing `describe`"),
        "{out}"
    );
    let out = with("impl Describe String { fun describe s = s  fun other s = s }\ndef main = 1\n");
    assert!(
        out.contains("`other` is not a method of `Describe`"),
        "{out}"
    );
    let out = with("impl Describe String { fun describe s = 5 }\ndef main = 1\n");
    assert!(out.contains("type mismatch"), "{out}");
    let out = with("impl Missing Int { }\ndef main = 1\n");
    assert!(out.contains("unknown trait `Missing`"), "{out}");
    let out = with("impl Maybe Int { }\ndef main = 1\n");
    assert!(out.contains("`Maybe` is a type, not a trait"), "{out}");
    let out = with("impl Describe (Maybe Int) { fun describe m = \"m\" }\ndef main = 1\n");
    assert!(
        out.contains("a type constructor applied to distinct variables"),
        "{out}"
    );
    let out = errors("trait Conv a { fun conv : a -> b -> b }\ndef main = 1\n");
    assert!(out.contains("has a type variable of its own"), "{out}");
    // Nothing says which `impl`: the value is made and thrown away.
    let out = errors(
        "trait Make a { fun make : () -> a }\nimpl Make Int { fun make u = 1 }\n\
         def main = let _ = make () in 0\n",
    );
    assert!(
        out.contains("cannot tell which `impl Make` is meant"),
        "{out}"
    );
}

// --- across modules and packages ---------------------------------------------

#[test]
fn a_trait_is_used_across_modules() {
    let out = common::eval_unit(&[
        (
            "Shape",
            "@pub(pkg) trait Area a { fun area : a -> Int }\n\
             @pub(pkg) record Square = { side : Int }\n\
             impl Area Square { fun area s = s.side * s.side }\n",
        ),
        (
            "Round",
            "use Shape (Area, area)\n\
             @pub(pkg) record Circle = { r : Int }\n\
             impl Area Circle { fun area c = 3 * c.r * c.r }\n\
             @pub(pkg) fun double x = 2 * area x\n",
        ),
        (
            "",
            "use Shape (Square, area)\nuse Round (Circle, double)\n\
             def main = (area (Square { side = 3 }), double (Circle { r = 2 }), double (Square { side = 1 }))\n",
        ),
    ]);
    assert_eq!(out, "(9, 24, 2)");
}

fn scratch(who: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("meadow-traits-{}-{who}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A dependent implements a package's trait for its own type, uses the
/// package's `impl`s and its bounded functions, and gets its defaults.
#[test]
fn a_trait_crosses_the_package() {
    let root = scratch("pkg");
    write(
        &root.join("Pretty/Meadow.toml"),
        "[package]\nname = \"Pretty\"\nversion = \"0.1.0\"\n",
    );
    write(
        &root.join("Pretty/src/Lib.mw"),
        "@pub trait Pretty a {\n\
         \x20 fun pretty : a -> String\n\
         \x20 fun framed : a -> String\n\
         \x20 fun framed x = \"[\" ++ pretty x ++ \"]\"\n\
         }\n\
         impl Pretty Int { fun pretty n = show n }\n\
         impl Pretty [a;] where Pretty a {\n\
         \x20 fun pretty xs = match xs with | [;] -> \"\" | x :: r -> pretty x ++ \";\" ++ pretty r\n\
         }\n\
         @pub fun shout x = framed x ++ \"!\"\n",
    );
    write(
        &root.join("App/Meadow.toml"),
        "[package]\nname = \"App\"\nversion = \"0.1.0\"\n\n[dependencies]\nPretty = { path = \"../Pretty\" }\n",
    );
    write(
        &root.join("App/src/Main.mw"),
        "use Pretty (Pretty, pretty, framed, shout)\n\
         record Point = { x : Int, y : Int }\n\
         impl Pretty Point { fun pretty p = pretty p.x ++ \"/\" ++ pretty p.y }\n\
         def main = (pretty [1; 2], framed (Point { x = 1, y = 2 }), shout [Point { x = 3, y = 4 };])\n",
    );
    for opts in [Options::debug(), Options::release()] {
        let out = pipeline::build(&root.join("App"), opts);
        assert!(
            out.diagnostics.is_empty(),
            "{:?}",
            out.diagnostics.iter().map(|d| &d.msg).collect::<Vec<_>>()
        );
        let program = out.linked.unwrap().program;
        let want = r#"("1;2;", "[1/2]", "[3/4;]!")"#;
        assert_eq!(meadow_eval::run(&program).unwrap().to_string(), want);
        assert_eq!(
            runtime::run(&program, Engine::Jit, OptLevel::O2).unwrap(),
            want
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

// --- several parameters --------------------------------------------------------

#[test]
fn a_trait_can_be_of_several_types() {
    let src = "trait Convert a b { fun convert : a -> b }\n\
        impl Convert Int String { fun convert n = \"#\" ++ show n }\n\
        impl Convert Int Bool { fun convert n = n != 0 }\n\
        impl Convert Bool Int { fun convert b = if b then 1 else 0 }\n\
        impl Convert [a;] [b;] where Convert a b {\n\
        \x20 fun convert xs = match xs with\n\
        \x20   | [;] -> [;]\n\
        \x20   | x :: rest -> convert x :: convert rest\n\
        }\n\
        trait Scale v s {\n\
        \x20 type Out v s\n\
        \x20 fun scale : s -> v -> Out v s\n\
        }\n\
        record V2 = { x : Int, y : Int }\n\
        impl Scale V2 Int {\n\
        \x20 type Out V2 Int = V2\n\
        \x20 fun scale k v = V2 { x = k * v.x, y = k * v.y }\n\
        }\n\
        impl Scale V2 Bool {\n\
        \x20 type Out V2 Bool = Int\n\
        \x20 fun scale b v = if b then v.x + v.y else 0\n\
        }\n\
        trait Same a { fun same : a -> a -> Bool }\n\
        impl Same Int { fun same x y = x == y }\n\
        trait RoundTrip a b where Convert a b, Convert b a, Same a {\n\
        \x20 fun stable : a -> b -> Bool\n\
        }\n\
        impl RoundTrip Int Bool { fun stable n witness = same (there (back n witness)) n }\n\
        fun back : a -> b -> b where Convert a b\n\
        fun back x witness = convert x\n\
        fun there : b -> a where Convert b a\n\
        fun there y = convert y\n\
        fun strings : [Int;] -> [String;]\n\
        fun strings xs = convert xs\n\
        fun both : a -> (b, c) where Convert a b, Convert a c\n\
        fun both x = (convert x, convert x)\n\
        fun viaRound : a -> b -> Bool where RoundTrip a b\n\
        fun viaRound x w = stable x w and same x x\n\
        fun pairOf : Int -> (String, Bool)\n\
        fun pairOf n = both n\n\
        fun flag : Bool -> Int\n\
        fun flag b = convert b\n\
        def main = (strings [1; 2], pairOf 0, flag True, scale 3 (V2 { x = 1, y = 2 }),\n\
        \x20 scale True (V2 { x = 1, y = 2 }), viaRound 1 True, viaRound 5 True)\n";
    is(
        src,
        r##"(["#1"; "#2"], ("#0", False), 1, V2(3, 6), 3, True, False)"##,
    );
    let out =
        errors("trait Convert a b { fun convert : a -> b }\nimpl Convert Int { }\ndef main = 1\n");
    assert!(
        out.contains("`Convert` is a trait of 2 types, given 1"),
        "{out}"
    );
    let out = errors(
        "trait Convert a b { fun convert : a -> b }\n\
         impl Convert Int String { fun convert n = show n }\n\
         fun f : Int -> Bool\nfun f n = convert n\ndef main = f 1\n",
    );
    assert!(
        out.contains("`Int Bool` does not implement `Convert`"),
        "{out}"
    );
}

// --- dictionaries, specialized away ----------------------------------------------

/// What a program reaches once its dictionaries are known takes none: the
/// copies are well typed, and the functions of dictionaries they were made
/// from are nowhere `main` can get to.
#[test]
fn known_dictionaries_are_specialized_away() {
    use meadow_compiler::core;
    let src = format!(
        "{DESCRIBE}fun loud : a -> String where Describe a\n\
         fun loud x = twice x ++ \"!\"\n\
         fun all xs = match xs with | [;] -> \"\" | x :: rest -> loud x ++ all rest\n\
         def main = (all [1; 2], all [[True;]; [False;]], loud (1, [2;]))\n"
    );
    let (program, diags) = pipeline::compile_str_with_std("test", &src, Options::debug());
    assert!(
        diags.is_empty(),
        "{:?}",
        diags.iter().map(|d| &d.msg).collect::<Vec<_>>()
    );
    let program = core::prune::prune(&program);
    for opt in [OptLevel::O1, OptLevel::O2] {
        let copied = core::dictionaries::program(&program, opt);
        let problems = core::lint::check(&copied, &copied.variants, &Default::default());
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        let reached = core::prune::prune(&copied);
        let takes_one = |d: &core::Def| match &d.poly.ty {
            meadow_compiler::infer::Type::Fun(params, _, _) => matches!(
                &params[0],
                meadow_compiler::infer::Type::Con(n, _) if n.ends_with("Describe")
            ),
            _ => false,
        };
        let left: Vec<String> = reached
            .defs
            .iter()
            .filter(|d| takes_one(d))
            .map(|d| d.name.to_string())
            .collect();
        assert!(
            left.is_empty(),
            "at {}: still taking a dictionary: {left:?}",
            opt.name()
        );
        assert!(
            reached.defs.len() < copied.defs.len(),
            "the generic originals are unreachable"
        );
    }
}

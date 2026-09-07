//! Type-inference snapshots: for a representative program, snapshot the inferred
//! scheme of every top-level binding (see `common::schemes`). A trailing `!! ...`
//! line means a diagnostic was produced.

mod common;
use common::schemes;

#[test]
fn literals_and_arithmetic() {
    insta::assert_snapshot!(schemes(
        "def n = 1 + 2 * 3\ndef s = \"hi\"\ndef u = ()\n"
    ));
}

#[test]
fn identity_generalizes() {
    insta::assert_snapshot!(schemes("def id = \\x -> x\n"));
}

#[test]
fn curried_functions() {
    insta::assert_snapshot!(schemes(
        "fun const a b = a\nfun compose f g x = f (g x)\n"
    ));
}

#[test]
fn let_polymorphism() {
    insta::assert_snapshot!(schemes(
        "def usePair =\n  let pair = \\a -> \\b -> (a, b) in\n  (pair 1 2, pair \"x\" \"y\")\n"
    ));
}

#[test]
fn recursion() {
    insta::assert_snapshot!(schemes(
        "fun fib n = if n == 0 then 0 else if n == 1 then 1 else fib (n - 1) + fib (n - 2)\n"
    ));
}

#[test]
fn if_and_match() {
    insta::assert_snapshot!(schemes(
        "fun classify n =\n  match n == 0 with\n  | True -> \"zero\"\n  | False -> \"nonzero\"\n"
    ));
}

#[test]
fn tuples_and_lists() {
    insta::assert_snapshot!(schemes(
        "fun swap p = match p with | (a, b) -> (b, a)\nfun singleton x = [x]\n"
    ));
}

#[test]
fn builtin_list_constructors() {
    insta::assert_snapshot!(schemes(
        "fun map f xs =\n  match xs with\n  | Nil -> Nil\n  | Cons x rest -> Cons (f x) (map f rest)\n"
    ));
}

#[test]
fn row_polymorphic_field_access() {
    insta::assert_snapshot!(schemes("def name = \\r -> r.name\n"));
}

#[test]
fn record_extension() {
    insta::assert_snapshot!(schemes("fun withAge r = { age = 0 | r }\n"));
}

#[test]
fn data_sum_type() {
    insta::assert_snapshot!(schemes(
        "data Shape = Circle Int | Rect Int Int\n\
         fun area s = match s with | Circle r -> r * r | Rect w h -> w * h\n"
    ));
}

#[test]
fn data_polymorphic_recursive() {
    insta::assert_snapshot!(schemes(
        "data Tree a = Tip | Branch (Tree a) a (Tree a)\n\
         fun size t = match t with | Tip -> 0 | Branch l x r -> 1 + size l + size r\n"
    ));
}

#[test]
fn nominal_record_and_field() {
    insta::assert_snapshot!(schemes(
        "record Person = { name : String, age : Int }\n\
         def p = Person { name = \"Ann\", age = 30 }\n\
         def who = p.name\n"
    ));
}

#[test]
fn maybe_type() {
    insta::assert_snapshot!(schemes(
        "data Maybe a = None | Some a\n\
         fun orElse m d = match m with | None -> d | Some x -> x\n"
    ));
}

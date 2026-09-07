//! End-to-end evaluation snapshots. Each program defines `main`; the snapshot is
//! the `Display` of the resulting `Value` (or the runtime error).

mod common;
use common::{eval_expr, eval_main};

#[test]
fn arithmetic() {
    insta::assert_snapshot!(eval_expr("1 + 2 * 3 - 4"));
}

#[test]
fn comparison_and_bool() {
    insta::assert_snapshot!(eval_expr("if 2 < 3 then True else False"));
}

#[test]
fn string_literal() {
    insta::assert_snapshot!(eval_expr("\"hello\""));
}

#[test]
fn tuple_and_list() {
    insta::assert_snapshot!(eval_expr("(1, [2, 3], \"x\")"));
}

#[test]
fn lambda_application() {
    insta::assert_snapshot!(eval_expr("(\\x -> \\y -> x + y) 10 20"));
}

#[test]
fn let_binding() {
    insta::assert_snapshot!(eval_expr("let x = 21 in x + x"));
}

#[test]
fn recursion_fib() {
    insta::assert_snapshot!(eval_main(
        "fun fib n = if n < 2 then n else fib (n - 1) + fib (n - 2)\n\
         def main = fib 15\n"
    ));
}

#[test]
fn list_map_builtin() {
    insta::assert_snapshot!(eval_main(
        "fun map f xs = match xs with | Nil -> Nil | Cons x r -> Cons (f x) (map f r)\n\
         def main = map (\\x -> x * x) [1, 2, 3, 4]\n"
    ));
}

#[test]
fn match_on_data() {
    insta::assert_snapshot!(eval_main(
        "data Shape = Circle Int | Rect Int Int\n\
         fun area s = match s with | Circle r -> r * r | Rect w h -> w * h\n\
         def main = area (Rect 3 4) + area (Circle 5)\n"
    ));
}

#[test]
fn recursive_data() {
    insta::assert_snapshot!(eval_main(
        "data Tree a = Tip | Branch (Tree a) a (Tree a)\n\
         fun sum t = match t with | Tip -> 0 | Branch l x r -> x + sum l + sum r\n\
         def main = sum (Branch (Branch Tip 3 Tip) 5 (Branch Tip 7 Tip))\n"
    ));
}

#[test]
fn nominal_record_field() {
    insta::assert_snapshot!(eval_main(
        "record Person = { name : String, age : Int }\n\
         def p = Person { name = \"Ann\", age = 30 }\n\
         def main = p.age\n"
    ));
}

#[test]
fn structural_record_field() {
    insta::assert_snapshot!(eval_expr("{ x = 1, y = 2 }.y"));
}

#[test]
fn division_by_zero() {
    insta::assert_snapshot!(eval_expr("1 / 0"));
}

#[test]
fn non_exhaustive_match() {
    insta::assert_snapshot!(eval_main(
        "fun head xs = match xs with | Cons x r -> x\n\
         def main = head Nil\n"
    ));
}

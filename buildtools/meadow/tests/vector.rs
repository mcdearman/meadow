//! `Std.Collections.Vector` — the persistent RRB vector behind `[…]` literals.
//! The prelude re-exports its sequence API unqualified.

mod common;
use common::{eval_expr_std, eval_main_std};

/// `mk n` = the vector `0,1,…,n-1` built with `pushBack`; `mkFront` with
/// `pushFront`; `sumV` folds.
const P: &str = "\
fun mk n =\n\
\x20 let rec go i v = if i >= n then v else go (i + 1) (pushBack v i)\n\
\x20 in go 0 empty\n\
fun mkFront n =\n\
\x20 let rec go i v = if i >= n then v else go (i + 1) (pushFront v i)\n\
\x20 in go 0 empty\n\
fun sumV v = foldl (\\a x -> a + x) 0 v\n";

fn run(body: &str) -> String {
    eval_main_std(&format!("{P}def main = {body}\n"))
}

#[test]
fn small_vector_roundtrips() {
    insta::assert_snapshot!(eval_expr_std("toList (fromArray #[1, 2, 3])"), @"[1; 2; 3]");
    insta::assert_snapshot!(eval_expr_std("len (fromArray #[1, 2, 3])"), @"3");
    insta::assert_snapshot!(eval_expr_std("toList empty"), @"[]");
    insta::assert_snapshot!(eval_expr_std("get (fromArray #[10, 20, 30]) 1"), @"Just(20)");
    insta::assert_snapshot!(eval_expr_std("get (fromArray #[10, 20, 30]) 9"), @"None");
    insta::assert_snapshot!(eval_expr_std("toList [1, 2, 3]"), @"[1; 2; 3]");
}

#[test]
fn push_back_builds_a_large_vector() {
    insta::assert_snapshot!(run("len (mk 1000)"), @"1000");
    insta::assert_snapshot!(run("(getOr 0 (mk 1000) 0, getOr 0 (mk 1000) 500, getOr 0 (mk 1000) 999)"), @"(0, 500, 999)");
    insta::assert_snapshot!(run("sumV (mk 1000)"), @"499500");
}

#[test]
fn push_front_reverses() {
    insta::assert_snapshot!(run("toList (mkFront 5)"), @"[4; 3; 2; 1; 0]");
    insta::assert_snapshot!(run("len (mkFront 200)"), @"200");
    insta::assert_snapshot!(run("(getOr 0 (mkFront 200) 0, getOr 0 (mkFront 200) 199)"), @"(199, 0)");
}

#[test]
fn pop_back_and_front() {
    insta::assert_snapshot!(run("toList (popBack (fromArray #[1, 2, 3]))"), @"[1; 2]");
    insta::assert_snapshot!(run("toList (popFront (fromArray #[1, 2, 3]))"), @"[2; 3]");
    insta::assert_snapshot!(run(
        "let rec dropN k v = if k <= 0 then v else dropN (k - 1) (popBack v) in len (dropN 900 (mk 1000))"
    ), @"100");
    insta::assert_snapshot!(run(
        "let rec dropN k v = if k <= 0 then v else dropN (k - 1) (popBack v) in toList (dropN 997 (mk 1000))"
    ), @"[0; 1; 2]");
    insta::assert_snapshot!(run(
        "let rec dropN k v = if k <= 0 then v else dropN (k - 1) (popFront v) in toList (dropN 997 (mk 1000))"
    ), @"[997; 998; 999]");
}

#[test]
fn set_is_persistent() {
    insta::assert_snapshot!(eval_main_std(
        "def a = fromArray #[1, 2, 3, 4]\n\
         def b = set a 1 99\n\
         def main = (toList a, toList b)\n"
    ), @"([1; 2; 3; 4], [1; 99; 3; 4])");
    insta::assert_snapshot!(run("getOr 0 (set (mk 1000) 777 (0 - 1)) 777"), @"-1");
    insta::assert_snapshot!(run("getOr 0 (set (mk 1000) 777 (0 - 1)) 776"), @"776");
}

#[test]
fn map_and_filter() {
    insta::assert_snapshot!(run("toList (map (\\x -> x * x) (fromArray #[1, 2, 3, 4]))"), @"[1; 4; 9; 16]");
    insta::assert_snapshot!(run("sumV (map (\\x -> x * 2) (mk 100))"), @"9900");
    insta::assert_snapshot!(run("toList (filter (\\x -> x % 3 == 0) (fromArray #[1, 3, 6, 7, 9]))"), @"[3; 6; 9]");
}

#[test]
fn append_split_slice() {
    insta::assert_snapshot!(run("toList (append (fromArray #[1, 2]) (fromArray #[3, 4, 5]))"), @"[1; 2; 3; 4; 5]");
    insta::assert_snapshot!(run("len (append (mk 500) (mk 500))"), @"1000");
    insta::assert_snapshot!(run("(getOr 0 (append (mk 500) (mk 300)) 499, getOr 0 (append (mk 500) (mk 300)) 500)"), @"(499, 0)");
    insta::assert_snapshot!(run("(\\p -> (toList (fst p), toList (snd p))) (splitAt (fromArray #[1, 2, 3, 4, 5]) 2)"), @"([1; 2], [3; 4; 5])");
    insta::assert_snapshot!(run("toList (slice (fromArray #[1, 2, 3, 4, 5]) 1 4)"), @"[2; 3; 4]");
}

#[test]
fn list_roundtrip() {
    insta::assert_snapshot!(eval_expr_std("toList (fromList (1 :: 2 :: 3 :: Nil))"), @"[1; 2; 3]");
    insta::assert_snapshot!(run("len (range 0 250)"), @"250");
}

#[test]
fn mixed_operations_agree_with_an_array() {
    // Every operation, in a pseudo-random mix, on vectors of up to about
    // 160 elements, checked element by element against an array. A short
    // leaf used to get into a radix-indexed tree -- from `fromArray`, or from a
    // `popBack` followed by pushes -- and indexing then went past its end.
    let src = "\
fun agrees v xs =\n\
\x20 let rec go i = if i >= arrayLen xs then True else getOr (0 - 1) v i == arrayGet xs i and go (i + 1) in\n\
\x20 len v == arrayLen xs and go 0\n\
fun step v xs k seed =\n\
\x20 let n = arrayLen xs in\n\
\x20 let j = if n == 0 then 0 else seed % n in\n\
\x20 match k with\n\
\x20 | 0 -> (pushBack v seed, arrayPush xs seed)\n\
\x20 | 1 -> (pushFront v seed, arrayConcat #[seed] xs)\n\
\x20 | 2 -> if n == 0 then (v, xs) else (popBack v, arraySlice xs 0 (n - 1))\n\
\x20 | 3 -> if n == 0 then (v, xs) else (popFront v, arraySlice xs 1 n)\n\
\x20 | 4 -> if n == 0 then (v, xs) else (set v j seed, arraySet xs j seed)\n\
\x20 | 5 -> (append (take v j) (drop v j), xs)\n\
\x20 | 6 -> (fromArray (toArray v), xs)\n\
\x20 | _ -> (append v (fromArray (arraySlice xs 0 j)), arrayConcat xs (arraySlice xs 0 j))\n\
fun run v xs i seed bad =\n\
\x20 if i >= 700 then bad\n\
\x20 else\n\
\x20   let s2 = (seed * 1103515245 + 12345) % 2147483648 in\n\
\x20   let k = if arrayLen xs > 160 then 2 + s2 % 2 else if arrayLen xs < 40 then s2 % 2 else s2 % 16 in\n\
\x20   match step v xs (if k > 7 then k % 2 else k) (s2 / 16) with\n\
\x20   | (v2, xs2) -> run v2 xs2 (i + 1) s2 (if agrees v2 xs2 then bad else bad + 1)\n\
def main = run empty #[] 0 7 0\n";
    assert_eq!(eval_main_std(src), "0");
}

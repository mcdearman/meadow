//! `Std.Either` — a value that is one of two things.

mod common;
use common::{eval_main_std, schemes_std};

fn e(body: &str) -> String {
    eval_main_std(&format!("use Std.Either as E\ndef main = {body}\n"))
}

// --- the eliminator ----------------------------------------------------------

#[test]
fn either_collapses_both_sides() {
    assert_eq!(e(r#"E.either (\n -> n + 1) (\s -> 0) (Left 5)"#), "6");
    assert_eq!(e(r#"E.either (\n -> n + 1) (\s -> 0) (Right "x")"#), "0");
}

#[test]
fn the_side_tests_and_defaults() {
    assert_eq!(e("E.isLeft (Left 1)"), "true");
    assert_eq!(e("E.isRight (Left 1)"), "false");
    assert_eq!(e("E.fromLeft 0 (Left 7)"), "7");
    assert_eq!(e("E.fromLeft 0 (Right 7)"), "0");
    assert_eq!(e("E.fromRight 0 (Right 7)"), "7");
    assert_eq!(e("E.fromRight 0 (Left 7)"), "0");
}

// --- mapping -----------------------------------------------------------------

#[test]
fn map_is_right_biased_and_map_left_is_the_other_one() {
    assert_eq!(e(r#"E.map (\x -> x + 1) (Right 1)"#), "Right(2)");
    assert_eq!(e(r#"E.map (\x -> x + 1) (Left 1)"#), "Left(1)");
    assert_eq!(e(r#"E.mapLeft (\x -> x + 1) (Left 1)"#), "Left(2)");
    assert_eq!(e(r#"E.mapLeft (\x -> x + 1) (Right 1)"#), "Right(1)");
}

#[test]
fn bimap_maps_whichever_side_is_there() {
    assert_eq!(e(r#"E.bimap (\x -> x + 1) (\s -> 0) (Left 1)"#), "Left(2)");
    assert_eq!(e(r#"E.bimap (\x -> x + 1) (\y -> y * 2) (Right 4)"#), "Right(8)");
}

#[test]
fn and_then_chains_on_the_right_and_short_circuits_on_the_left() {
    assert_eq!(e(r#"E.andThen (\x -> Right (x + 1)) (Right 1)"#), "Right(2)");
    assert_eq!(e(r#"E.andThen (\x -> Right (x + 1)) (Left "no")"#), r#"Left("no")"#);
    // A `Left` produced by the function is kept.
    assert_eq!(e(r#"E.andThen (\x -> Left "no") (Right 1)"#), r#"Left("no")"#);
}

#[test]
fn swap_exchanges_the_sides() {
    assert_eq!(e("E.swap (Left 1)"), "Right(1)");
    assert_eq!(e("E.swap (Right 1)"), "Left(1)");
}

// --- conversions -------------------------------------------------------------

#[test]
fn conversions_to_and_from_maybe_and_result() {
    assert_eq!(e("E.toMaybe (Right 1)"), "Just(1)");
    assert_eq!(e("E.toMaybe (Left 1)"), "None");
    assert_eq!(e("E.leftToMaybe (Left 1)"), "Just(1)");
    assert_eq!(e("E.leftToMaybe (Right 1)"), "None");
    assert_eq!(e(r#"E.toResult (Left "bad")"#), r#"Err("bad")"#);
    assert_eq!(e("E.toResult (Right 1)"), "Ok(1)");
    assert_eq!(e(r#"E.fromResult (Err "bad")"#), r#"Left("bad")"#);
    assert_eq!(e("E.fromResult (Ok 1)"), "Right(1)");
    assert_eq!(e(r#"E.fromMaybe "none" (Just 1)"#), "Right(1)");
    assert_eq!(e(r#"E.fromMaybe "none" None"#), r#"Left("none")"#);
}

#[test]
fn result_round_trips_through_either() {
    assert_eq!(e("E.toResult (E.fromResult (Ok 1))"), "Ok(1)");
    assert_eq!(e(r#"E.toResult (E.fromResult (Err "e"))"#), r#"Err("e")"#);
}

// --- collections -------------------------------------------------------------

#[test]
fn lefts_rights_and_partition_keep_their_order() {
    let es = r#"[Left 1; Right "a"; Left 2; Right "b";]"#;
    assert_eq!(e(&format!("E.lefts {es}")), "[1; 2]");
    assert_eq!(e(&format!("E.rights {es}")), r#"["a"; "b"]"#);
    assert_eq!(
        e(&format!("E.partitionEithers {es}")),
        r#"([1; 2], ["a"; "b"])"#
    );
    // Empty and one-sided inputs.
    assert_eq!(e("E.partitionEithers [;]"), "([], [])");
    assert_eq!(e("E.partitionEithers [Left 1;]"), "([1], [])");
}

// --- types -------------------------------------------------------------------

#[test]
fn the_schemes_are_what_they_should_be() {
    assert_eq!(
        schemes_std(
            "use Std.Either (either, map, mapLeft, bimap, swap, partitionEithers)\n\
             fun a f g x = either f g x\n\
             fun b f x = map f x\n\
             fun c f x = mapLeft f x\n\
             fun d f g x = bimap f g x\n\
             fun e2 x = swap x\n\
             fun f2 xs = partitionEithers xs\n"
        ),
        "a : forall a b e c. (a -> b ! e) -> (c -> b ! e) -> Either a c -> b ! e\n\
         b : forall a b e c. (a -> b ! e) -> Either c a -> Either c b ! e\n\
         c : forall a b e c. (a -> b ! e) -> Either a c -> Either b c ! e\n\
         d : forall a b e c d. (a -> b ! e) -> (c -> d ! e) -> Either a c -> Either b d ! e\n\
         e2 : forall a b. Either a b -> Either b a\n\
         f2 : forall a b. List (Either a b) -> (List a, List b)\n"
    );
}

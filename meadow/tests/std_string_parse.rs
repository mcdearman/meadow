//! `Std.String.Parse` — the megaparsec-style combinators.
//!
//! The interesting behaviour to pin down is **consumed input**: `alt` commits to
//! a branch that has advanced the offset, `try` undoes that, and `chunk` is
//! atomic. Everything else is ordinary combinator plumbing.

mod common;
use common::eval_main_std;

/// Evaluate `body` with `Std.Parser` in scope as `P` and a few helpers defined.
fn parse(body: &str) -> String {
    eval_main_std(&format!(
        "use Std.String.Parse as P\n\
         use Std.Result (Result, Ok, Err)\n\
         def isDigit = \\c -> c >= 48 and c <= 57\n\
         def number = P.map (\\ds -> foldl (\\a d -> a * 10 + (d - 48)) 0 ds) \
                            (P.takeWhile1P \"a digit\" isDigit)\n\
         def comma = P.single 44\n\
         fun describe r = match r with | Ok v -> \"ok\" | Err e -> P.showError e\n\
         def main = {body}\n"
    ))
}

// --- the basics --------------------------------------------------------------

#[test]
fn takes_a_run_of_matching_tokens() {
    assert_eq!(
        parse("P.runParser (P.thenSkip number P.eof) (P.fromString \"1234\")"),
        "Ok(1234)"
    );
}

#[test]
fn eof_rejects_a_trailing_remainder() {
    assert_eq!(
        parse("describe (P.runParser (P.thenSkip number P.eof) (P.fromString \"12x\"))"),
        "\"at offset 2, expected end of input\""
    );
    // ...but the same parser without `eof` is happy to stop early.
    assert_eq!(
        parse("P.runParserPartial number (P.fromString \"12x\")"),
        "Ok((12, 2))"
    );
}

#[test]
fn take_while1p_needs_at_least_one() {
    assert_eq!(
        parse("describe (P.runParser number (P.fromString \"x\"))"),
        "\"at offset 0, expected a digit\""
    );
}

#[test]
fn a_label_replaces_what_was_expected() {
    assert_eq!(
        parse(
            "describe (P.runParser (P.label \"an identifier\" (P.single 97)) \
             (P.fromString \"z\"))"
        ),
        "\"at offset 0, expected an identifier\""
    );
}

// --- consumed input, which is the whole point --------------------------------

#[test]
fn alt_commits_to_a_branch_that_consumed() {
    // `lz` eats the `l` before failing on `z`, so `alt` will not reconsider.
    assert_eq!(
        parse(
            "let lz = P.skipThen (P.single 108) (P.single 122) in \
             describe (P.runParser (P.alt lz (P.single 108)) (P.fromString \"let\"))"
        ),
        "\"at offset 1, expected a different token\""
    );
}

#[test]
fn try_restores_backtracking() {
    assert_eq!(
        parse(
            "let lz = P.skipThen (P.single 108) (P.single 122) in \
             describe (P.runParser (P.alt (P.try lz) (P.single 108)) (P.fromString \"let\"))"
        ),
        "\"ok\""
    );
}

#[test]
fn chunk_is_atomic_so_a_shared_prefix_needs_no_try() {
    assert_eq!(
        parse(
            "describe (P.runParser \
               (P.alt (P.chunk (P.fromString \"letter\")) (P.chunk (P.fromString \"let\"))) \
               (P.fromString \"lets\"))"
        ),
        "\"ok\""
    );
}

#[test]
fn alt_unions_what_both_branches_wanted() {
    // Both fail at offset 0 without consuming, so the error lists both labels.
    assert_eq!(
        parse(
            "describe (P.runParser \
               (P.alt (P.label \"a\" (P.single 97)) (P.label \"b\" (P.single 98))) \
               (P.fromString \"z\"))"
        ),
        "\"at offset 0, expected a or b\""
    );
}

#[test]
fn the_error_that_got_furthest_wins() {
    // The right branch fails at offset 1, past the left branch's offset 0.
    assert_eq!(
        parse(
            "let ab = P.try (P.skipThen (P.single 97) (P.label \"b\" (P.single 98))) in \
             describe (P.runParser (P.alt ab (P.label \"z\" (P.single 122))) \
                       (P.fromString \"ax\"))"
        ),
        "\"at offset 1, expected b\""
    );
}

// --- repetition --------------------------------------------------------------

#[test]
fn many_stops_at_the_first_non_match() {
    assert_eq!(
        parse("P.runParserPartial (P.many (P.single 97)) (P.fromString \"aaab\")"),
        "Ok(([97, 97, 97], 3))"
    );
    // `many` accepts none at all.
    assert_eq!(
        parse("P.runParser (P.many (P.single 97)) (P.fromString \"b\")"),
        "Ok([])"
    );
}

#[test]
fn some_needs_at_least_one() {
    assert_eq!(
        parse("describe (P.runParser (P.some (P.single 98)) (P.fromString \"aaa\"))"),
        "\"at offset 0, expected a different token\""
    );
}

#[test]
fn many_reports_a_parser_that_never_consumes() {
    // `many (pure 1)` would loop forever; it is an error instead of a hang.
    assert_eq!(
        parse("describe (P.runParser (P.many (P.pure 1)) (P.fromString \"a\"))"),
        "\"at offset 0, expected a parser that consumes input\""
    );
}

#[test]
fn count_takes_exactly_n() {
    assert_eq!(
        parse("P.runParser (P.count 2 (P.single 97)) (P.fromString \"aaa\")"),
        "Ok([97, 97])"
    );
    assert_eq!(
        parse("describe (P.runParser (P.count 4 (P.single 97)) (P.fromString \"aaa\"))"),
        "\"at offset 3 (end of input), expected any token\""
    );
}

#[test]
fn separated_and_bracketed() {
    assert_eq!(
        parse("P.runParser (P.sepBy number comma) (P.fromString \"1,22,333\")"),
        "Ok([1, 22, 333])"
    );
    // `sepBy` accepts nothing at all; `sepBy1` does not.
    assert_eq!(
        parse("P.runParser (P.sepBy number comma) (P.fromString \"\")"),
        "Ok([])"
    );
    assert_eq!(
        parse(
            "P.runParser (P.between (P.single 40) (P.single 41) (P.sepBy number comma)) \
             (P.fromString \"(1,2)\")"
        ),
        "Ok([1, 2])"
    );
}

#[test]
fn many_till_stops_at_the_terminator() {
    assert_eq!(
        parse(
            "P.runParser (P.manyTill P.anySingle (P.single 59)) (P.fromString \"ab;\")"
        ),
        "Ok([97, 98])"
    );
}

// --- lookahead ---------------------------------------------------------------

#[test]
fn look_ahead_rewinds_on_success() {
    assert_eq!(
        parse(
            "P.runParserPartial (P.lookAhead (P.single 97)) (P.fromString \"ab\")"
        ),
        "Ok((97, 0))"
    );
}

#[test]
fn not_followed_by_succeeds_only_when_absent() {
    assert_eq!(
        parse("P.runParser (P.notFollowedBy (P.single 98)) (P.fromString \"ab\")"),
        "Ok(())"
    );
    assert_eq!(
        parse("describe (P.runParser (P.notFollowedBy (P.single 97)) (P.fromString \"ab\"))"),
        "\"at offset 0, expected no match here\""
    );
}

#[test]
fn optional_yields_none_without_consuming() {
    assert_eq!(
        parse("P.runParserPartial (P.optional (P.single 97)) (P.fromString \"b\")"),
        "Ok((None, 0))"
    );
    assert_eq!(
        parse("P.runParserPartial (P.optional (P.single 97)) (P.fromString \"ab\")"),
        "Ok((Just(97), 1))"
    );
}

// --- a whole grammar ---------------------------------------------------------

#[test]
fn a_recursive_grammar_via_defer() {
    // sum := atom ('+' atom)*  ;  atom := number | '(' sum ')'
    //
    // `defer` is what lets the grammar refer to itself: definitions are
    // evaluated eagerly, so writing `sum` directly inside `atom` would loop
    // while the definitions were being built.
    let src = "\
use Std.String.Parse as P\n\
def number = P.map (\\ds -> foldl (\\a d -> a * 10 + (d - 48)) 0 ds) \
                   (P.takeWhile1P \"a digit\" (\\c -> c >= 48 and c <= 57))\n\
fun sum u = \
  P.lift2 (\\x xs -> foldl (\\a b -> a + b) x xs) \
          (P.defer atom) \
          (P.many (P.skipThen (P.single 43) (P.defer atom)))\n\
fun atom u = P.alt number (P.between (P.single 40) (P.single 41) (P.defer sum))\n\
def main = P.runParser (P.thenSkip (P.defer sum) P.eof) (P.fromString \"1+(2+3)+4\")\n";
    assert_eq!(eval_main_std(src), "Ok(10)");
}

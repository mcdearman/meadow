//! `Std.String.Parse` — the megaparsec-style combinators, over streams.
//!
//! The interesting behaviour to pin down is **consumed input**: `alt` commits to
//! a branch that has advanced the offset, `try` undoes that, and `chunk` is
//! atomic. Then what the three stream traits buy: parsing any `impl Stream`,
//! messages through `VisualStream` (derivable when the type displays well),
//! and lines and columns through `TraversableStream`.

mod common;
use common::eval_main_std;

/// Evaluate `body` with the parser library in scope and a few helpers defined.
fn parse(body: &str) -> String {
    eval_main_std(&format!(
        r#"use Std.String.Parse as P
use Std.String.Parse.Char as C
use Std.String.Parse.Lexer as L
use Std.Result.Result.*
use Std.Char (isDigit)
fun number = P.map (\ds -> foldl (\a d -> a * 10 + (charCode d - 48)) 0 (P.chunkToTokens ds)) (P.takeWhile1P "a digit" isDigit)
fun comma = C.char ','
fun describe r = match r with | Ok v -> "ok" | Err e -> P.showError e
def main = {body}
"#
    ))
}

// --- the basics --------------------------------------------------------------

#[test]
fn takes_a_run_of_matching_tokens() {
    assert_eq!(
        parse(r#"P.runParser (P.thenSkip number P.eof) "1234""#),
        "Ok(1234)"
    );
}

#[test]
fn eof_rejects_a_trailing_remainder() {
    assert_eq!(
        parse(r#"describe (P.runParser (P.thenSkip number P.eof) "12x")"#),
        r#""at offset 2: unexpected 'x'; expecting end of input or a digit""#
    );
    // ...but the same parser without `eof` is happy to stop early.
    assert_eq!(parse(r#"P.runParserPartial number "12x""#), "Ok((12, 2))");
}

#[test]
fn take_while1p_needs_at_least_one() {
    assert_eq!(
        parse(r#"describe (P.runParser number "x")"#),
        r#""at offset 0: unexpected 'x'; expecting a digit""#
    );
}

#[test]
fn a_label_replaces_what_was_expected() {
    assert_eq!(
        parse(r#"describe (P.runParser (P.label "an identifier" (C.char 'a')) "z")"#),
        r#""at offset 0: unexpected 'z'; expecting an identifier""#
    );
}

// --- consumed input, which is the whole point --------------------------------

#[test]
fn alt_commits_to_a_branch_that_consumed() {
    // `lz` eats the `l` before failing on `z`, so `alt` will not reconsider.
    assert_eq!(
        parse(
            r#"let lz = P.skipThen (C.char 'l') (C.char 'z') in
               describe (P.runParser (P.alt lz (C.char 'l')) "let")"#
        ),
        r#""at offset 1: unexpected 'e'; expecting 'z'""#
    );
}

#[test]
fn try_restores_backtracking() {
    assert_eq!(
        parse(
            r#"let lz = P.skipThen (C.char 'l') (C.char 'z') in
               describe (P.runParser (P.alt (P.try lz) (C.char 'l')) "let")"#
        ),
        r#""ok""#
    );
}

#[test]
fn chunk_is_atomic_so_a_shared_prefix_needs_no_try() {
    assert_eq!(
        parse(r#"describe (P.runParser (P.alt (C.string "letter") (C.string "let")) "lets")"#),
        r#""ok""#
    );
}

#[test]
fn alt_unions_what_both_branches_wanted() {
    // Both fail at offset 0 without consuming, so the error lists both labels.
    assert_eq!(
        parse(
            r#"describe (P.runParser (P.alt (P.label "a" (C.char 'a')) (P.label "b" (C.char 'b'))) "z")"#
        ),
        r#""at offset 0: unexpected 'z'; expecting a or b""#
    );
}

#[test]
fn the_error_that_got_furthest_wins() {
    // The left branch fails at offset 1, past the right branch's offset 0.
    assert_eq!(
        parse(
            r#"let ab = P.try (P.skipThen (C.char 'a') (P.label "b" (C.char 'b'))) in
               describe (P.runParser (P.alt ab (P.label "z" (C.char 'z'))) "ax")"#
        ),
        r#""at offset 1: unexpected 'x'; expecting b""#
    );
}

// --- repetition --------------------------------------------------------------

#[test]
fn many_stops_at_the_first_non_match() {
    assert_eq!(
        parse(r#"P.runParserPartial (P.many (C.char 'a')) "aaab""#),
        "Ok((['a', 'a', 'a'], 3))"
    );
    // `many` accepts none at all.
    assert_eq!(parse(r#"P.runParser (P.many (C.char 'a')) "b""#), "Ok([])");
}

#[test]
fn some_needs_at_least_one() {
    assert_eq!(
        parse(r#"describe (P.runParser (P.some (C.char 'b')) "aaa")"#),
        r#""at offset 0: unexpected 'a'; expecting 'b'""#
    );
}

#[test]
fn many_reports_a_parser_that_never_consumes() {
    // `many (pure 1)` would loop forever; it is an error instead of a hang.
    assert_eq!(
        parse(r#"describe (P.runParser (P.many (P.pure 1)) "a")"#),
        r#""at offset 0: a repeated parser that consumes nothing""#
    );
}

#[test]
fn count_takes_exactly_n() {
    assert_eq!(
        parse(r#"P.runParser (P.count 2 (C.char 'a')) "aaa""#),
        "Ok(['a', 'a'])"
    );
    assert_eq!(
        parse(r#"describe (P.runParser (P.count 4 (C.char 'a')) "aaa")"#),
        r#""at offset 3: unexpected end of input; expecting 'a'""#
    );
}

#[test]
fn separated_and_bracketed() {
    assert_eq!(
        parse(r#"P.runParser (P.sepBy number comma) "1,22,333""#),
        "Ok([1, 22, 333])"
    );
    // `sepBy` accepts nothing at all; `sepBy1` does not.
    assert_eq!(parse(r#"P.runParser (P.sepBy number comma) """#), "Ok([])");
    assert_eq!(
        parse(
            r#"P.runParser (P.between (C.char '(') (C.char ')') (P.sepBy number comma)) "(1,2)""#
        ),
        "Ok([1, 2])"
    );
}

#[test]
fn many_till_stops_at_the_terminator() {
    assert_eq!(
        parse(r#"P.runParser (P.manyTill P.anySingle (C.char ';')) "ab;""#),
        "Ok(['a', 'b'])"
    );
}

// --- lookahead ---------------------------------------------------------------

#[test]
fn look_ahead_rewinds_on_success() {
    assert_eq!(
        parse(r#"P.runParserPartial (P.lookAhead (C.char 'a')) "ab""#),
        "Ok(('a', 0))"
    );
}

#[test]
fn not_followed_by_succeeds_only_when_absent() {
    assert_eq!(
        parse(r#"P.runParser (P.notFollowedBy (C.char 'b')) "ab""#),
        "Ok(())"
    );
    assert_eq!(
        parse(r#"describe (P.runParser (P.notFollowedBy (C.char 'a')) "ab")"#),
        r#""at offset 0: unexpected 'a'""#
    );
}

#[test]
fn optional_yields_none_without_consuming() {
    assert_eq!(
        parse(r#"P.runParserPartial (P.optional (C.char 'a')) "b""#),
        "Ok((None, 0))"
    );
    assert_eq!(
        parse(r#"P.runParserPartial (P.optional (C.char 'a')) "ab""#),
        "Ok((Just('a'), 1))"
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
    let src = r#"use Std.String.Parse as P
use Std.String.Parse.Lexer as L
use Std.String.Parse.Char as C
fun plus = P.mapTo (\a b -> a + b) (C.char '+')
fun sum u = P.chainl1 (P.defer atom) plus
fun atom u = P.alt L.decimal (P.between (C.char '(') (C.char ')') (P.defer sum))
def main = P.runParser (P.thenSkip (P.defer sum) P.eof) "1+(2+3)+4"
"#;
    assert_eq!(eval_main_std(src), "Ok(10)");
}

#[test]
fn a_lexer_skips_space_and_comments_between_tokens() {
    assert_eq!(
        parse(
            r#"let sc = L.space C.space1 (L.skipLineComment ";") P.empty in
               P.runParser (P.skipThen sc (P.many (L.lexeme sc (L.signed sc L.decimal)))) " 1 ; one\n -2 3""#
        ),
        "Ok([1, -2, 3])"
    );
}

// --- the stream traits -------------------------------------------------------

#[test]
fn an_error_is_a_span_of_the_source_and_a_message() {
    // Offsets, not lines and columns: showing it to a person is the caller's.
    assert_eq!(
        parse(
            r#"match P.parse (P.skipThen (C.string "let x = ") number) "let x = y" with
               | Ok _ -> ((0, 0), "no error")
               | Err e -> e"#
        ),
        r#"((8, 9), "unexpected 'y'\nexpecting a digit\n")"#
    );
}

#[test]
fn a_vector_of_tokens_is_a_stream_and_shows_them_by_display() {
    // A lexer's output: the tokens render as their `Display` says.
    let src = r#"use Std.String.Parse as P
use Std.Result.Result.*
use Tok.*
@derive(PartialEq)
data Tok = Num Int | Plus
impl Display Tok {
  fun display t = match t with | Num n -> "number ${n}" | Plus -> "'+'"
}
def main =
  match P.runParser (P.thenSkip (P.single Plus) P.eof) [Plus, Num 1] with
  | Ok _ -> "no error"
  | Err e -> P.parseErrorTextPretty e
"#;
    assert_eq!(
        eval_main_std(src),
        r#""unexpected number 1\nexpecting end of input\n""#
    );
}

#[test]
fn a_stream_of_ones_own_derives_its_visual_stream() {
    // `Stream` by hand; `VisualStream` by `@derive`, which renders each token by
    // its `Display` -- and compiles only because an `Int` has one. Its
    // `TraversableStream` maps a token's offset to the span of text it came
    // from, which is how an error in a token stream points back at the source.
    let src = r#"use Std.String.Parse as P
use Std.Result.Result.*
use Std.Collections.Vector as V
use Std.String.Parse (Stream, VisualStream, TraversableStream, Token, take1, takeN, takeWhile, tokenToChunk, tokensToChunk, chunkToTokens, chunkLength, sourceSpan)
use Digits.*

-- Each digit, with the span of the source it was read from.
@derive(VisualStream)
data Digits = Digits [(Int, (Int, Int))]

fun items d = match d with | Digits xs -> xs

fun spanAt d i = match V.get (items d) i with | Just (_, sp) -> sp | None -> (100, 100)

impl Stream Digits {
  type Token Digits = Int
  fun take1 d off = match V.get (items d) off with | Just (x, _) -> Just (x, off + 1) | None -> None
  fun takeN d n off = if off + n > V.len (items d) then None else Just (Digits (V.slice (items d) off (off + n)), off + n)
  fun takeWhile d f off = (Digits [], off)
  fun tokenToChunk x = Digits [(x, (0, 0))]
  fun tokensToChunk xs = Digits (V.map (\x -> (x, (0, 0))) xs)
  fun chunkToTokens d = V.map (\p -> match p with | (x, _) -> x) (items d)
  fun chunkLength d = V.len (items d)
}

impl TraversableStream Digits {
  fun sourceSpan d from to =
    match (spanAt d from, spanAt d (to - 1)) with
    | ((start, _), (_, stop)) -> if to > from then (start, stop) else (start, start)
}

def main =
  match P.parse (P.skipThen (P.single 1) (P.single 7)) (Digits [(1, (0, 3)), (3, (10, 14))]) with
  | Ok _ -> ((0, 0), "no error")
  | Err e -> e
"#;
    assert_eq!(
        eval_main_std(src),
        r#"((10, 14), "unexpected 3\nexpecting 7\n")"#
    );
}

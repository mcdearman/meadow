//! Operators a program defines: `fun (<+>) a b = ...`, fixity declarations,
//! and the language's own operators as the methods of `Std.Ops`'s traits.

mod common;
use common::{
    cek_main_std, errors, eval_expr_std, eval_main, eval_main_std, eval_unit, schemes_std,
    unit_errors,
};

#[test]
fn a_fixity_declaration_says_how_an_operator_groups() {
    let left = "infixl 6 <+>\nfun (<+>) a b = a * 10 + b\ndef main = 1 <+> 2 <+> 3\n";
    assert_eq!(eval_main(left), "123");
    let right = "infixr 6 <+>\nfun (<+>) a b = a * 10 + b\ndef main = 1 <+> 2 <+> 3\n";
    assert_eq!(eval_main(right), "33");
}

#[test]
fn a_fixity_declaration_says_how_tightly_it_binds() {
    // At `+`'s level it is looser than `*`; above `*`'s, tighter; and with no
    // declaration at all it is `infixl 9`, tighter than every other.
    let at = |fixity: &str| {
        eval_main(&format!(
            "{fixity}fun (<+>) a b = a - b\ndef main = 10 * 2 <+> 3\n"
        ))
    };
    assert_eq!(at("infixl 6 <+>\n"), "17");
    assert_eq!(at("infixl 8 <+>\n"), "-10");
    assert_eq!(at(""), "-10");
}

#[test]
fn the_languages_operators_keep_their_precedence() {
    assert_eq!(eval_expr_std("1 + 2 * 3 - 4"), "3");
    assert_eq!(eval_expr_std("2 ^ 3 ^ 2"), "512");
    assert_eq!(eval_expr_std("10 - 3 - 2"), "5");
    assert_eq!(eval_expr_std("1 :: 2 :: [;]"), "[1; 2]");
    assert_eq!(eval_expr_std("1 + 2 < 4"), "True");
    assert_eq!(eval_expr_std("-2 ^ 2"), "4");
    assert_eq!(eval_expr_std("1 << 4 + 1"), "17");
}

#[test]
fn an_operator_in_parentheses_is_a_name() {
    assert_eq!(eval_expr_std("(+) 4 5"), "9");
    assert_eq!(eval_expr_std("foldl (*) 1 [1, 2, 3, 4]"), "24");
    assert_eq!(eval_expr_std("(==) [1] [1]"), "True");
}

#[test]
fn without_std_an_operator_is_its_primitive() {
    assert_eq!(
        eval_main("def main = (1 + 2 * 3, (-) 7 2, 3 < 4)\n"),
        "(7, 5, True)"
    );
}

#[test]
fn comparisons_do_not_group() {
    let e = errors("def main = 1 == 2 == True\n");
    assert!(e.contains("does not group"), "{e}");
    let e = errors("def main = 1 < 2 == True\n");
    assert!(e.contains("cannot be mixed without parentheses"), "{e}");
    assert_eq!(eval_expr_std("(1 < 2) == True"), "True");
}

#[test]
fn a_declaration_may_not_change_the_languages_fixities() {
    let e = errors("infixr 3 +\ndef main = 1\n");
    assert!(
        e.contains("`+` is `infixl 6` in the language itself"),
        "{e}"
    );
    // Repeating it is fine: `Std.Ops` does.
    assert_eq!(eval_main("infixl 6 +\ndef main = 1 + 1\n"), "2");
}

#[test]
fn two_declarations_of_one_operator_must_agree() {
    let e = errors("infixl 3 <+>\ninfixr 3 <+>\ndef main = 1\n");
    assert!(e.contains("already declared `infixl 3`"), "{e}");
    let e = errors("infixl 12 <+>\ndef main = 1\n");
    assert!(e.contains("out of range"), "{e}");
}

#[test]
fn a_fixity_holds_in_every_module_of_the_package() {
    let modules = [
        (
            "",
            "mod Ops\nmod User\nuse User (answer)\ndef main = answer\n",
        ),
        ("Ops", "infixr 5 <+>\n@pub fun (<+>) a b = a * 10 + b\n"),
        ("User", "use Ops ((<+>))\n@pub def answer = 1 <+> 2 <+> 3\n"),
    ];
    assert_eq!(unit_errors(&modules), "");
    assert_eq!(eval_unit(&modules), "33");
}

#[test]
fn a_type_of_ones_own_gets_an_operator_with_an_impl() {
    let src = "\
record V2 = { x : Int, y : Int }

impl Add V2 {
  fun (+) a b = V2 { x = a.x + b.x, y = a.y + b.y }
}

fun sumAll xs = foldl (+) (V2 { x = 0, y = 0 }) xs

def main = (V2 { x = 1, y = 2 } + V2 { x = 3, y = 4 }, sumAll [V2 { x = 1, y = 1 }, V2 { x = 2, y = 2 }])
";
    assert_eq!(eval_main_std(src), "(V2(4, 6), V2(3, 3))");
    assert_eq!(cek_main_std(src), "(V2(4, 6), V2(3, 3))");
}

#[test]
fn an_impl_of_partial_eq_gives_a_type_its_equality() {
    let src = "\
data Loose = Loose Int

impl PartialEq Loose {
  fun (==) a b = True
}

def main = (Loose 1 == Loose 2, Loose 1 != Loose 2, Just 1 == Just 2)
";
    assert_eq!(eval_main_std(src), "(True, False, False)");
    assert_eq!(cek_main_std(src), "(True, False, False)");
}

#[test]
fn a_generic_function_asks_for_the_operators_it_uses() {
    let src = "fun square x = x * x\nfun same x y = x == y\n";
    assert_eq!(
        schemes_std(src),
        "square : forall a. Mul a => a -> a\nsame : forall a. PartialEq a => a -> a -> Bool\n"
    );
}

#[test]
fn a_type_without_the_operator_is_told_so() {
    let e = common::run_main_std("def main = \"a\" + \"b\"\n", meadow::Engine::Vm);
    assert!(e.contains("`String` does not implement `Add`"), "{e}");
}

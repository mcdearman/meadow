//! Transactions, as the type checker sees them.
//!
//! A transaction may perform only `Stm`, which is what makes running it again
//! on a conflict safe. What transactions do at run time is in
//! `rts/tests/differential.rs`, on both the VM and the CEK machine.

mod common;
use common::{errors_std_with, schemes_std};

fn errors(src: &str) -> String {
    errors_std_with(src, meadow::Options::debug())
}

#[test]
fn atomically_takes_a_transaction_and_performs_thread() {
    let out = schemes_std("use Std.Stm as S\ndef a = S.atomically\ndef r = S.readTVar\n");
    assert!(
        out.contains("a : forall a e. (() -> a ! Stm) -> a ! { Thread | e }"),
        "{out}"
    );
    assert!(
        out.contains("r : forall a e. TVar a -> a ! { Stm | e }"),
        "{out}"
    );
}

#[test]
fn a_transaction_cannot_print() {
    // Running it again on a conflict would print again.
    let src = "use Std.Stm as S\n\
               fun f tv = S.atomically (\\() -> let _ = println \"hi\" in S.readTVar tv)\n";
    assert!(
        errors(src).contains("the effect `Console` is not allowed here"),
        "{}",
        errors(src)
    );
}

#[test]
fn a_transaction_cannot_touch_a_ref() {
    let src = "use Std.Stm as S\n\
               fun f r tv = S.atomically (\\() -> let _ = setRef r 1 in S.readTVar tv)\n";
    assert!(
        errors(src).contains("the effect `Mut` is not allowed here"),
        "{}",
        errors(src)
    );
}

#[test]
fn transactions_do_not_nest() {
    let src = "use Std.Stm as S\n\
               fun f tv = S.atomically (\\() -> S.atomically (\\() -> S.readTVar tv))\n";
    assert!(
        errors(src).contains("the effect `Thread` is not allowed here"),
        "{}",
        errors(src)
    );
}

#[test]
fn a_transaction_may_use_local_state() {
    let src = "use Std.Stm as S\n\
               fun f tv = S.atomically (\\() -> runSt (\\() -> let r = stNewRef (toInt 1) in stGetRef r) + S.readTVar tv)\n";
    assert_eq!(errors(src), "");
}

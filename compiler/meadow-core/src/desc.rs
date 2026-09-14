//! **Descriptors: what a value of a type variable's type is, at run time.**
//!
//! Code generic over `a` does not know, when it is compiled, whether an `a` is
//! an address, an `Int` or a `Float` -- and once values stop saying what they
//! are, the collector and the structural primitives need to be told. So a
//! definition generic over a type variable takes one hidden argument per
//! variable, its *descriptor*: a small integer naming the representation the
//! variable is instantiated to. An instantiation passes one; code under the
//! abstraction reads it.
//!
//! A descriptor describes the value, not the whole type. A `List Float` is
//! [`REF`] -- what is inside it is the object's own business -- so descriptors
//! never need building at run time: an instantiation at a known type passes a
//! constant, and one at a type variable passes the descriptor it was given.
//!
//! A release build that specializes generic code away has nothing left to
//! pass. See `meadow_seq::lower` for where they are threaded.

use crate::Ty;
use crate::num::Width;

/// A descriptor, as the integer a register holds.
pub type Desc = i64;

/// An address: data, a closure, an array, a record, a `BigInt`, a `Ref`.
pub const REF: Desc = 0;
pub const INT: Desc = 1;
pub const FLOAT: Desc = 2;
pub const STR: Desc = 3;
pub const UNIT: Desc = 4;
pub const BOOL: Desc = 5;
pub const CHAR: Desc = 6;
pub const FLOAT32: Desc = 7;
/// The first sized integer: [`WORD`] plus the width's place in [`Width::ALL`].
pub const WORD: Desc = 8;
/// Nothing is known: a type the compiler could not work out, or a variable
/// no enclosing abstraction binds. Whatever the value says it is.
pub const ANY: Desc = 15;

/// The descriptor of a sized integer type.
pub fn word(w: Width) -> Desc {
    let place = Width::ALL
        .iter()
        .position(|x| *x == w)
        .expect("every width is in ALL");
    WORD + place as Desc
}

/// The descriptor of values of type `ty`, or `None` for a type variable, whose
/// descriptor is whatever the abstraction binding it was given.
pub fn of(ty: &Ty) -> Option<Desc> {
    Some(match ty {
        Ty::Var(_) => return None,
        Ty::Con(n, args) if args.is_empty() => match &**n {
            "Int" | "Int64" => INT,
            "Float" | "Float64" => FLOAT,
            "String" => STR,
            "Unit" => UNIT,
            "Bool" => BOOL,
            "Char" => CHAR,
            "Float32" => FLOAT32,
            "?" => ANY,
            name => match Width::from_type(name) {
                Some(w) => word(w),
                None => REF,
            },
        },
        Ty::Con(..) | Ty::Tuple(_) | Ty::Record(_) | Ty::Fun(..) => REF,
        _ => ANY,
    })
}

/// What a descriptor is called, for a message.
pub fn name(d: Desc) -> String {
    match d {
        REF => "a reference".into(),
        INT => "Int".into(),
        FLOAT => "Float".into(),
        STR => "String".into(),
        UNIT => "Unit".into(),
        BOOL => "Bool".into(),
        CHAR => "Char".into(),
        FLOAT32 => "Float32".into(),
        ANY => "anything".into(),
        d if (WORD..WORD + Width::ALL.len() as Desc).contains(&d) => {
            Width::ALL[(d - WORD) as usize].name().into()
        }
        d => format!("descriptor {d}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use meadow_intern::InternedString;

    fn con(n: &str) -> Ty {
        Ty::Con(InternedString::from(n), Vec::new())
    }

    #[test]
    fn a_value_is_described_by_its_outermost_type() {
        assert_eq!(of(&con("Int")), Some(INT));
        assert_eq!(of(&con("UInt8")), Some(word(Width::U8)));
        assert_eq!(
            of(&Ty::Con(InternedString::from("List"), vec![con("Float")])),
            Some(REF)
        );
        assert_eq!(of(&Ty::Var(3)), None);
        assert_eq!(name(word(Width::U64)), "UInt64");
    }
}

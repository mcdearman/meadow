//! Descriptors, as the runtimes define them (`meadow_rt::desc`), and the one
//! thing only a front end can say about them: which descriptor a type has.

pub use meadow_rt::desc::*;

use crate::Ty;
use crate::num::Width;

/// The descriptor of values of type `ty`, or `None` for a type variable, whose
/// descriptor is whatever the abstraction binding it was given.
pub fn of(ty: &Ty) -> Option<Desc> {
    Some(match ty {
        Ty::Var(_) => return None,
        Ty::Con(n, args) if args.is_empty() => match &**n {
            "Int" | "Int64" => INT,
            "Float" | "Float64" => FLOAT,
            SYMBOL_TYPE => STR,
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
    }
}

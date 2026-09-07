//! String interning.
//!
//! Identifiers, string literals and type names are interned into a single
//! process-wide [`ThreadedRodeo`] so the rest of the compiler can pass around a
//! `Copy`, `Eq`, `Hash` handle ([`InternedString`]) instead of `String`s.
//! `InternedString` `Deref`s to `str`, so it is usable directly in `format!`,
//! comparisons, etc. The interner is never cleared — fine for a compiler process.

use lasso::{Spur, ThreadedRodeo};
use once_cell::sync::Lazy;
use std::{
    borrow::Borrow,
    fmt::{Debug, Display},
    ops::Deref,
};

static INTERNER: Lazy<ThreadedRodeo> = Lazy::new(|| ThreadedRodeo::default());

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct InternedString {
    pub key: Spur,
}

impl From<Spur> for InternedString {
    fn from(key: Spur) -> Self {
        Self { key }
    }
}

impl From<&str> for InternedString {
    fn from(name: &str) -> Self {
        Self {
            key: INTERNER.get_or_intern(name),
        }
    }
}

impl From<String> for InternedString {
    fn from(name: String) -> Self {
        Self {
            key: INTERNER.get_or_intern(name),
        }
    }
}

impl Debug for InternedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InternedString({})", INTERNER.resolve(&self.key))
    }
}

impl Display for InternedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", INTERNER.resolve(&self.key))
    }
}

impl Borrow<str> for InternedString {
    fn borrow(&self) -> &str {
        INTERNER.resolve(&self.key)
    }
}

impl Deref for InternedString {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        INTERNER.resolve(&self.key)
    }
}

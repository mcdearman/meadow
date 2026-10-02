//! The names an AxCut program binds: numbers, as a register file's are.
//!
//! A front end hands them out -- Meadow numbers them as it numbers its own
//! variables, a compilation unit at a time -- and the top of the range is
//! kept for names a driver invents after compiling.

/// Where the synthetic range begins.
///
/// Everything below is handed out by a compilation unit, through [`VarIdGen`].
/// The top is for variables invented *after* compilation — a driver appending
/// an entry point to a program it has already linked — which belong to no unit
/// and must not collide with one.
pub const SYNTHETIC_BASE: u32 = 0x7000_0000;

#[derive(
    Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct VarId(pub u32);

impl VarId {
    /// The `n`th variable invented after compilation — see [`SYNTHETIC_BASE`].
    ///
    /// Indexed by the caller rather than counted here, so that appending the
    /// same definitions to the same program twice produces the same ids.
    pub const fn synthetic(n: u32) -> Self {
        VarId(SYNTHETIC_BASE + n)
    }

    /// Was this invented after compilation rather than by a unit?
    pub const fn is_synthetic(self) -> bool {
        self.0 >= SYNTHETIC_BASE
    }
}

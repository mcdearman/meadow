//! Source buffers.
//!
//! A [`Source`] is a `Copy` handle bundling an id, its origin ([`SourceKind`] —
//! a file path or the interactive prompt) and its interned contents, so it can be
//! passed by value through the lexer/parser. [`Sources`] is the `ariadne` cache
//! used to render diagnostics with source snippets.

use meadow_intern::InternedString;
use meadow_span::Span;
use std::{ops::Index, sync::atomic::AtomicU32};

pub type SourceId = u32;

pub struct Sources {
    entries: Vec<(String, ariadne::Source<String>)>,
}

impl ariadne::Cache<SourceId> for &Sources {
    type Storage = String;

    fn fetch(
        &mut self,
        id: &SourceId,
    ) -> Result<&ariadne::Source<Self::Storage>, impl std::fmt::Debug> {
        self.entries
            .get(*id as usize)
            .map(|(_, s)| s)
            .ok_or_else(|| format!("unregistered file id {}", id))
    }

    fn display<'b>(&self, id: &'b SourceId) -> Option<impl std::fmt::Display + 'b> {
        Some(self.entries[*id as usize].0.clone())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    pub content: InternedString,
}

static SOURCE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Ids from here up are fixed: a standard library module's is its place in the
/// library, the same in every process. A compiled `Std` is kept and read back
/// by other processes, and what it compiled to -- a debugger's locations --
/// names its sources by id, so those ids have to mean the same thing there.
pub const FIXED_IDS: SourceId = u32::MAX - (1 << 16);

/// A source is saved as where it came from and what it says. An id made by
/// [`Source::new`] is only unique within the process that made it, so one
/// read back gets a new one; a fixed id ([`Source::fixed`]) is kept.
impl serde::Serialize for Source {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        (self.id, self.kind, self.content).serialize(s)
    }
}

impl<'de> serde::Deserialize<'de> for Source {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let (id, kind, content) = <(SourceId, SourceKind, InternedString)>::deserialize(d)?;
        Ok(if id >= FIXED_IDS {
            Source { id, kind, content }
        } else {
            Source::new(kind, content)
        })
    }
}

impl Source {
    pub fn new(kind: SourceKind, content: InternedString) -> Self {
        let id = SOURCE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { id, kind, content }
    }

    /// The source whose id is the `index`th fixed one -- see [`FIXED_IDS`].
    pub fn fixed(index: u32, kind: SourceKind, content: InternedString) -> Self {
        Self {
            id: FIXED_IDS + index,
            kind,
            content,
        }
    }

    pub fn name(&self) -> InternedString {
        match &self.kind {
            SourceKind::File(n) => n.clone(),
            SourceKind::Interactive => "<interactive>".into(),
        }
    }

    pub fn len(&self) -> usize {
        self.content.len()
    }
}

impl Index<usize> for Source {
    type Output = str;

    fn index(&self, index: usize) -> &Self::Output {
        &self.content[index..index + 1]
    }
}

impl Index<Span> for Source {
    type Output = str;

    fn index(&self, span: Span) -> &Self::Output {
        let r: std::ops::Range<usize> = span.into();
        &self.content[r]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SourceKind {
    File(InternedString),
    Interactive,
}

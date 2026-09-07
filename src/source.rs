//! Source buffers.
//!
//! A [`Source`] is a `Copy` handle bundling an id, its origin ([`SourceKind`] —
//! a file path or the interactive prompt) and its interned contents, so it can be
//! passed by value through the lexer/parser. [`Sources`] is the `ariadne` cache
//! used to render diagnostics with source snippets.

use crate::intern::InternedString;
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

impl Source {
    pub fn new(kind: SourceKind, content: InternedString) -> Self {
        let id = SOURCE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { id, kind, content }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    File(InternedString),
    Interactive,
}

use crate::intern::InternedString;
use std::ops::Index;

pub type SourceId = u32;

pub struct Sources {
    entries: Vec<(String, Source)>,
}

impl ariadne::Cache<SourceId> for &Sources {
    type Storage = String;

    fn fetch(&mut self, id: &SourceId) -> Result<&Source, impl std::fmt::Debug> {
        self.entries
            .get(id.0 as usize)
            .map(|(_, s)| s)
            .ok_or_else(|| format!("unregistered file id {}", id.0))
    }

    fn display<'b>(&self, id: &'b SourceId) -> Option<impl std::fmt::Display + 'b> {
        Some(self.entries[id.0 as usize].0.clone())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    pub content: InternedString,
}

impl Source {
    pub fn new(kind: SourceKind, content: InternedString) -> Self {
        Self { kind, content }
    }

    pub fn filename(&self) -> InternedString {
        match &self.kind {
            SourceKind::File(name) => name.clone(),
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

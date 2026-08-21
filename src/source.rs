use crate::intern::InternedString;
use std::ops::Index;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Source {
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

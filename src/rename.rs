use std::{collections::HashMap, sync::atomic::AtomicUsize};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResId(usize);

impl ResId {
    pub fn fresh() -> Self {
        Self(COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Env {
    frames: Vec<Frame>,
}

impl Env {
    pub fn new() -> Self {
        Self {
            frames: vec![Frame::new()],
        }
    }

    pub fn new_with_builtins(builtins: HashMap<ResId, String>) -> Self {
        let mut bindings = HashMap::new();

        for (id, name) in builtins {
            bindings.insert(name, id);
        }

        Self {
            frames: vec![Frame { bindings }],
        }
    }

    fn flatten(&self) -> HashMap<String, ResId> {
        self.frames
            .iter()
            .rev()
            .flat_map(|frame| frame.bindings.iter())
            .map(|(name, id)| (name.clone(), *id))
            .collect()
    }

    pub fn push(&mut self) {
        self.frames.push(Frame::new());
    }

    pub fn pop(&mut self) {
        self.frames.pop();
    }

    pub fn define(&mut self, name: String) -> ResId {
        if let Some(frame) = self.frames.last_mut() {
            frame.define(name)
        } else {
            let mut frame = Frame::new();
            let id = frame.define(name);
            self.frames.push(frame);
            id
        }
    }

    pub fn push_and_define(&mut self, name: String) -> ResId {
        let mut frame = Frame::new();
        let id = frame.define(name);
        self.frames.push(frame);
        id
    }

    pub fn find(&self, name: &String) -> Option<ResId> {
        for frame in self.frames.iter().rev() {
            if let Some(id) = frame.get(name) {
                return Some(id);
            }
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    bindings: HashMap<String, ResId>,
}

impl Frame {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
        }
    }

    pub fn define(&mut self, name: String) -> ResId {
        let id = ResId::fresh();
        self.bindings.insert(name, id);
        id
    }

    pub fn insert(&mut self, name: String, id: ResId) {
        self.bindings.insert(name, id);
    }

    pub fn get(&self, name: &String) -> Option<ResId> {
        self.bindings.get(name).copied()
    }
}

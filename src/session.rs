use crate::pipeline::Pipeline;

#[derive(Debug)]
pub struct Session {
    compiler_env: Pipeline,
}

impl Session {
    pub fn new() -> Self {
        Self {
            compiler_env: Pipeline::new(),
        }
    }
}

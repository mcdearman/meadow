#[derive(Debug)]
pub struct Session {
    compiler_env: Pipeline,
}

impl Session {
    pub fn new() -> Self {
        Self {
            resolver: Resolver::new_with_prelude(),
        }
    }
}

use crate::{lexer::Lexer, parser::parse, span::Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    File(String),
    Interactive,
}

#[derive(Debug, Clone)]
pub struct Pipeline<'src> {
    src: &'src str,
    mode: InputMode,
    lexer: Lexer<'src>,
}

impl<'src> Pipeline<'src> {
    pub fn new(src: &'src str, mode: InputMode) -> Self {
        Self {
            src,
            mode,
            lexer: Lexer::new(src),
        }
    }

    pub fn run(self) -> Result<(), String> {
        // for token in self.lexer.clone() {
        //     println!("{:?}", token);
        // }

        let (ast, errors) = parse(self.lexer, self.mode);
        println!("{:#?}", ast);
        Ok(())
    }
}

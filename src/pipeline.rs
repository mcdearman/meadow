use crate::lexer::Lexer;
use chumsky::input::Stream;

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
        let stream = Stream::from_iter(self.lexer);
        
        // let (ast, errors) = parse(stream, true);
        Ok(())
    }
}

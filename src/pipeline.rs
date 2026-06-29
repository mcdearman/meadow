use ariadne::Source;

use crate::{
    diagnostics::parse_report, intern::InternedString, lexer::Lexer, parser::parse, span::Span,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    File(InternedString),
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

    pub fn run(&self) -> Result<(), String> {
        // for token in self.lexer.clone() {
        //     println!("{:?}", token);
        // }

        let (ast, errors) = parse(self.lexer.clone(), self.mode);
        if errors.is_empty() {
            println!("{:#?}", ast);
        } else {
            // for error in errors {
            //     eprintln!("Error: {}", error);
            // 2. Prepare Ariadne's source cache so it can read your src string
            let filename = match self.mode {
                InputMode::File(name) => name,
                InputMode::Interactive => "<interactive>".into(),
            };

            let cache = (filename.to_string(), Source::from(self.src));

            for error in errors {
                // 3. Extract a primary label message and the span from the chumsky error.
                // Chumsky's Rich error provides `.reason()` and `.span()`
                let msg = error.to_string();
                let primary_span = *error.span();
                let label_text = format!("Unexpected token or syntax error: {}", error.reason());

                // 4. Build the report using your function
                let report = parse_report(
                    msg,
                    filename.to_string(),
                    (label_text, primary_span),
                    &error,
                );

                // 5. Print the report directly to stderr
                let _ = report.eprint(cache.clone());
            }
        }
        Ok(())
    }
}

use std::println;

use crate::{
    diagnostics::{build_report, parse_report},
    lexer::Lexer,
    parser::parse,
    rename::Resolver,
};
use ariadne::Source;

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
        let (ast, errors) = parse(self.lexer.clone(), &self.mode);
        if errors.is_empty() {
            // println!("{:#?}", ast);
            let mut resolver = Resolver::with_prelude();
            let (hir, res_errs) = resolver.resolve(&ast);
            if res_errs.is_empty() {
                println!("{:#?}", hir);
            } else {
                let filename = match &self.mode {
                    InputMode::File(name) => name.clone(),
                    InputMode::Interactive => "<interactive>".into(),
                };
                let cache = (filename.clone(), Source::from(self.src));
                for e in res_errs {
                    let report = build_report(e);
                    let _ = report.eprint(cache.clone());
                }
            }
        } else {
            let filename = match &self.mode {
                InputMode::File(name) => name.clone(),
                InputMode::Interactive => "<interactive>".into(),
            };

            let cache = (filename.clone(), Source::from(self.src));

            for error in errors {
                let msg = error.to_string();
                let primary_span = *error.span();
                let label_text = format!("Parse error: {}", error.reason());

                let report =
                    parse_report(msg, filename.clone(), (label_text, primary_span), &error);

                let _ = report.eprint(cache.clone());
            }
        }
        Ok(())
    }
}

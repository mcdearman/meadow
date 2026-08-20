use crate::{
    diagnostics::{build_report, parse_report},
    intern::InternedString,
    lexer::Lexer,
    parser::parse,
    rename::Resolver,
};
use ariadne::Source;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    File(InternedString),
    Interactive,
}

#[derive(Debug, Clone)]
pub struct Pipeline<'src> {
    pub src: &'src str,
    pub mode: InputMode,
    pub lexer: Lexer<'src>,
    pub resolver: Resolver,
}

impl<'src> Pipeline<'src> {
    pub fn new(src: &'src str, mode: InputMode) -> Self {
        Self {
            src,
            mode: mode.clone(),
            lexer: Lexer::new(src),
            resolver: Resolver::new_with_prelude(mode),
        }
    }

    pub fn new_with_context(src: &'src str, ctx: Self) -> Self {
        Self {
            src,
            mode: ctx.mode.clone(),
            lexer: Lexer::new(src),
            resolver: ctx.resolver.clone(),
        }
    }

    pub fn run(&mut self) -> Result<(), String> {
        let (ast, errors) = parse(self.lexer.clone(), &self.mode);
        if errors.is_empty() {
            // println!("{:#?}", ast);
            let (hir, res_errs) = self.resolver.resolve(&ast);
            if res_errs.is_empty() {
                println!("{:#?}", hir);
            } else {
                let filename = match &self.mode {
                    InputMode::File(name) => name.to_string(),
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
                InputMode::File(name) => name.to_string(),
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

use crate::{
    diagnostics::{build_report, parse_report},
    intern::InternedString,
    lexer::tokenize,
    parser::parse,
    rename::Resolver,
    source::{Source, SourceKind},
};

#[derive(Debug, Clone)]
pub struct Pipeline {
    pub src: Source,
    pub resolver: Resolver,
}

impl Pipeline {
    pub fn new(src: Source) -> Self {
        Self {
            src,
            resolver: Resolver::new_with_prelude(src),
        }
    }

    pub fn run(&mut self) -> Result<(), String> {
        let lex_res = tokenize(self.src);
        if !lex_res.errors.is_empty() {
            for error in lex_res.errors {
                let msg = error.to_string();
                let primary_span = *error.span();
                let label_text = format!(": {}", error.reason()),

                let report = parse_report(
                    msg,
                    self.src.filename().to_string(),
                    (label_text, primary_span),
                    &error,
                );

                let _ = report.eprint(cache.clone());
            }
        }
        let (ast, errors) = parse(self.src, &lex_res.tokens);
        if errors.is_empty() {
            // println!("{:#?}", ast);
            let (hir, res_errs) = self.resolver.resolve(&ast);
            if res_errs.is_empty() {
                println!("{:#?}", hir);
            } else {
                let cache = (
                    self.src.filename().to_string(),
                    ariadne::Source::from(self.src.content.to_string()),
                );
                for e in res_errs {
                    let report = build_report(e);
                    let _ = report.eprint(cache.clone());
                }
            }
        } else {
            let cache = (
                self.src.filename().to_string(),
                ariadne::Source::from(self.src.content.to_string()),
            );

            for error in errors {
                let msg = error.to_string();
                let primary_span = *error.span();
                let label_text = format!("Parse error: {}", error.reason());

                let report = parse_report(
                    msg,
                    self.src.filename().to_string(),
                    (label_text, primary_span),
                    &error,
                );

                let _ = report.eprint(cache.clone());
            }
        }
        Ok(())
    }
}

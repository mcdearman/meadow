use rustyline::{
    Completer, Editor, Helper, Highlighter, Hinter, error::ReadlineError, validate::Validator,
};

use crate::{
    pipeline::{InputMode, Pipeline},
    rename::Resolver,
};

#[derive(Completer, Helper, Highlighter, Hinter)]
struct TermValidator;

impl Validator for TermValidator {
    fn validate(
        &self,
        ctx: &mut rustyline::validate::ValidationContext,
    ) -> rustyline::Result<rustyline::validate::ValidationResult> {
        if ctx.input().ends_with("\n") {
            Ok(rustyline::validate::ValidationResult::Valid(None))
        } else {
            Ok(rustyline::validate::ValidationResult::Incomplete)
        }
    }
}

const BANNER: &str = r#"
      __  ___               __             
     /  |/  /__  ____ _____/ /___ _      __
    / /|_/ / _ \/ __ `/ __  / __ \ | /| / /
   / /  / /  __/ /_/ / /_/ / /_/ / |/ |/ / 
  /_/  /_/\___/\__,_/\__,_/\____/|__/|__/  

  Welcome to the Meadow REPL!
  Type :quit or :q to exit.                                        
"#;

pub struct Session {
    resolver: Resolver,
}

impl Session {
    pub fn new(mode: InputMode) -> Self {
        Self {
            resolver: Resolver::new_with_prelude(mode),
        }
    }

    pub fn run(&mut self) {
        env_logger::init();

        let h = TermValidator;
        let mut rl = Editor::new().expect("Failed to create editor");
        rl.set_helper(Some(h));
        if rl.load_history(".repl_history").is_err() {
            eprintln!("No previous history.");
        }

        println!("{}", BANNER);

        loop {
            let readline = rl.readline("> ");
            match readline {
                Ok(line) => {
                    match line.trim() {
                        ":q" | ":quit" => break,
                        "clear" => {
                            rl.clear_history().expect("history failed to clear");
                            continue;
                        }
                        _ => (),
                    }
                    rl.add_history_entry(line.as_str())
                        .expect("Failed to add history entry");

                    // self.pipeline.run();
                    // self.pipeline = Pipeline::new_with_context(&line, self.pipeline.clone());

                }
                Err(ReadlineError::Interrupted) => {
                    println!("CTRL-C");
                    break;
                }
                Err(ReadlineError::Eof) => {
                    println!("CTRL-D");
                    break;
                }
                Err(err) => {
                    println!("Error: {:?}", err);
                    break;
                }
            }
        }
        rl.save_history(".repl_history")
            .expect("Failed to save history");
    }
}

//! The interactive driver.
//!
//! The compiler has no notion of a REPL. We fake one here: every entry is compiled
//! as a throwaway one-module package whose dependencies are all the previous
//! entries (plus, transitively, whatever they depended on). The "repl prefix" is
//! literally handed back to the compiler as ordinary dependency packages.

use meadow::stdlib;
use meadow_compiler::{
    ast, core, compile_unit, diagnostics, hir,
    intern::InternedString,
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
    span::{Located, Span},
    AstModule, CompiledPackage,
};
use meadow_eval as eval;
use itertools::Either;
use rustyline::{
    error::ReadlineError,
    validate::{ValidationResult, Validator},
    Cmd, Completer, Editor, Helper, Highlighter, Hinter, KeyCode, KeyEvent, Modifiers,
};

#[derive(Completer, Helper, Highlighter, Hinter)]
struct TermValidator;

impl Validator for TermValidator {
    fn validate(
        &self,
        ctx: &mut rustyline::validate::ValidationContext,
    ) -> rustyline::Result<ValidationResult> {
        Ok(if needs_continuation(ctx.input()) {
            ValidationResult::Incomplete
        } else {
            ValidationResult::Valid(None)
        })
    }
}

/// Decide whether the current buffer should keep accepting lines instead of being
/// submitted.
///
/// - `:` commands and a trailing `\` are handled up front.
/// - On the **first** line, submit as soon as it is a complete parse (so `1 + 2`
///   needs a single Enter); otherwise keep editing.
/// - Once the entry spans multiple lines it is **blank-line terminated** (like
///   GHCi's `:set +m`), except that a buffer which already parses cleanly and has
///   nothing dangling is accepted early. Match/`data` arm lines (`| …`) always wait
///   for the blank line, since `match x with | A -> a` is itself a valid parse.
fn needs_continuation(input: &str) -> bool {
    if input.trim_start().starts_with(':') {
        return false;
    }
    if input.trim_end_matches([' ', '\t']).ends_with('\\') {
        return true;
    }

    let multiline = input.contains('\n');
    if multiline && input.ends_with('\n') {
        return false; // blank line submits a multi-line entry
    }
    if !multiline {
        return !parses_ok(input);
    }

    let last_line = input
        .rsplit('\n')
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    if last_line.trim_start().starts_with('|') {
        return true; // writing match / data arms — end with a blank line
    }

    let code = strip_strings_and_comments(input);
    if bracket_depth(&code) > 0 {
        return true;
    }
    if matches!(
        code.split_whitespace().last().unwrap_or(""),
        "->" | "=" | "==" | "!=" | "<=" | ">=" | "&&" | "||"
            | "let" | "in" | "if" | "then" | "else" | "match" | "with" | "fun" | "def"
            | "|" | "\\" | "+" | "-" | "*" | "/" | "%" | "^" | "<" | ">" | "." | ","
    ) {
        return true;
    }

    !parses_ok(input)
}

/// Whether `input` lexes and parses (as one decl or one expression) with no errors.
fn parses_ok(input: &str) -> bool {
    let src = Source::new(
        SourceKind::Interactive,
        InternedString::from(input.to_string()),
    );
    let lex = tokenize(src);
    if !lex.errors.is_empty() {
        return false;
    }
    let (parsed, errors) = parser::parse_repl(src, &lex.tokens);
    parsed.is_some() && errors.is_empty()
}

fn strip_strings_and_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(' ');
                while let Some(d) = chars.next() {
                    if d == '\\' {
                        chars.next();
                    } else if d == '"' {
                        break;
                    }
                }
                out.push(' ');
            }
            '-' if chars.peek() == Some(&'-') => {
                while let Some(&d) = chars.peek() {
                    if d == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Turn `foo \<newline> bar` into `foo <newline> bar` (drop the continuation marker,
/// keep the newline so spans stay sane).
fn join_continuations(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('\\') {
        let after = &rest[pos + 1..];
        let ws_end = after.len() - after.trim_start_matches([' ', '\t']).len();
        if after[ws_end..].starts_with('\n') {
            out.push_str(&rest[..pos]);
            rest = &after[ws_end..]; // keep the '\n'
        } else {
            out.push_str(&rest[..pos + 1]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn bracket_depth(code: &str) -> i32 {
    let mut depth = 0i32;
    for c in code.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

const BANNER: &str = r#"
      __  ___               __
     /  |/  /__  ____ _____/ /___ _      __
    / /|_/ / _ \/ __ `/ __  / __ \ | /| / /
   / /  / /  __/ /_/ / /_/ / /_/ / |/ |/ /
  /_/  /_/\___/\__,_/\__,_/\____/|__/|__/

  Meadow REPL.
  :q quit   :t <expr> type-check   :module list bindings   :reset
  Multi-line: Alt+Enter / Ctrl+J insert a newline; an unfinished line (open
  bracket, dangling operator, trailing `\`, `| ...` arms) keeps reading.
  End a multi-line entry with a blank line.
"#;

pub struct Session {
    line: u32,
    /// Compiled prior entries, oldest first — the dependency chain. The first
    /// `std_len` entries are the embedded `Std` package and survive `:reset`.
    prefix: Vec<CompiledPackage>,
    std_len: usize,
}

impl Session {
    pub fn new() -> Self {
        let (std_pkgs, diags) = stdlib::compile_std();
        if !diags.is_empty() {
            // A broken embedded prelude is a compiler bug, not a user error.
            for d in &diags {
                eprintln!("internal error compiling Std: {}: {}", d.filename, d.msg);
            }
        }
        let std_len = std_pkgs.len();
        Session {
            line: std_len as u32,
            prefix: std_pkgs,
            std_len,
        }
    }

    pub fn run(&mut self) {
        let _ = env_logger::try_init();

        let mut rl: Editor<TermValidator, rustyline::history::FileHistory> =
            Editor::new().expect("failed to create editor");
        rl.set_helper(Some(TermValidator));

        // Insert a literal newline instead of submitting. `Alt+Enter` and `Ctrl+J`
        // are portable and distinguishable; `Shift+Enter` only reaches us on
        // terminals that report the modifier (many send it identical to Enter, in
        // which case the validator's heuristics still give multi-line editing).
        for key in [
            KeyEvent(KeyCode::Enter, Modifiers::ALT),
            KeyEvent(KeyCode::Enter, Modifiers::SHIFT),
            KeyEvent::ctrl('J'),
        ] {
            rl.bind_sequence(key, Cmd::Newline);
        }

        let _ = rl.load_history(".repl_history");

        println!("{BANNER}");

        loop {
            match rl.readline("> ") {
                Ok(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let _ = rl.add_history_entry(line.as_str());
                    // Fold `\`-continued lines back together, then work with the
                    // whole (possibly multi-line) entry.
                    let entry = join_continuations(&line);
                    let trimmed = entry.trim();
                    match trimmed {
                        ":q" | ":quit" => break,
                        ":reset" => {
                            self.prefix.truncate(self.std_len);
                            self.line = self.std_len as u32;
                            println!("(reset)");
                        }
                        ":module" => self.list_module(),
                        _ if trimmed.starts_with(":t ") || trimmed.starts_with(":t\n") => {
                            self.handle(trimmed[2..].trim(), Mode::TypeOnly);
                        }
                        _ => self.handle(trimmed, Mode::Run),
                    }
                }
                Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => break,
                Err(e) => {
                    eprintln!("error: {e:?}");
                    break;
                }
            }
        }

        let _ = rl.save_history(".repl_history");
    }

    fn list_module(&self) {
        let user = &self.prefix[self.std_len.min(self.prefix.len())..];
        if user.is_empty() {
            println!("(no bindings yet)");
            return;
        }
        for pkg in user {
            for e in &pkg.exports {
                println!("{} : {}", e.name, e.scheme);
            }
        }
    }

    fn handle(&mut self, input: &str, mode: Mode) {
        let src = Source::new(
            SourceKind::Interactive,
            InternedString::from(input.to_string()),
        );

        let lex = tokenize(src);
        let (parsed, perrs) = parser::parse_repl(src, &lex.tokens);

        // Surface lex / parse problems with a source snippet, then bail if the
        // entry didn't parse at all.
        let mut front_errors = lex.errors.clone();
        front_errors.extend(perrs.iter().map(|e| diagnostics::from_parse_error("repl", e)));
        diagnostics::emit(&front_errors, "repl", input);
        let Some(item) = parsed else { return };

        let decl = match item {
            Either::Left(decl) => decl,
            Either::Right(expr) => synth_def("it", expr),
        };

        let module = Located::new(
            ast::Module {
                name: InternedString::from("repl"),
                decls: vec![decl],
            },
            Span::default(),
        );
        let ast_mod = AstModule {
            path: vec![],
            name: InternedString::from("repl"),
            ast: module,
        };

        let deps: Vec<&CompiledPackage> = self.prefix.iter().collect();
        let (compiled, diags) = compile_unit(
            InternedString::from(format!("repl:{}", self.line)),
            self.line as usize,
            vec![ast_mod],
            &deps,
        );

        let had_error = !diags.is_empty();
        diagnostics::emit(&diags, "repl", input);

        for e in &compiled.exports {
            println!("{} : {}", e.name, e.scheme);
        }
        if compiled.exports.is_empty() && !had_error {
            for d in &compiled.data_decls {
                match d.value() {
                    hir::Decl::Data(dd) => println!("defined type {}", dd.name),
                    hir::Decl::Record(rd) => println!("defined type {}", rd.name),
                    _ => {}
                }
            }
        }

        if mode == Mode::Run && !had_error {
            let entry = compiled.exports.last().map(|e| e.var);
            if entry.is_some() {
                let program = self.program_for(&compiled, entry);
                match eval::run(&program) {
                    Ok(value) => println!("= {value}"),
                    Err(e) => eprintln!("{e}"),
                }
            }
        }

        if mode == Mode::Run && !had_error {
            // Thread this entry into later lines (including redefinitions of `it`).
            self.prefix.push(compiled);
            self.line += 1;
        }
    }

    fn program_for(
        &self,
        current: &CompiledPackage,
        entry: Option<core::Var>,
    ) -> core::Program {
        let mut defs = Vec::new();
        let mut ctor_fields = std::collections::HashMap::new();
        for p in &self.prefix {
            defs.extend(p.defs.iter().cloned());
            ctor_fields.extend(p.ctor_fields.clone());
        }
        defs.extend(current.defs.iter().cloned());
        ctor_fields.extend(current.ctor_fields.clone());
        core::Program {
            defs,
            entry,
            ctor_fields,
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(PartialEq)]
enum Mode {
    Run,
    TypeOnly,
}

fn synth_def(name: &str, expr: ast::LExpr) -> ast::LDecl {
    let span = expr.span;
    let pat = Located::new(
        ast::Pat::Var(Located::new(InternedString::from(name), span)),
        span,
    );
    Located::new(
        ast::Decl::Bind(ast::Bind::Pat(pat, expr)),
        span,
    )
}

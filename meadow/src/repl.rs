//! The interactive driver.
//!
//! The compiler has no notion of a REPL. We fake one here: every entry is compiled
//! as a throwaway one-module package whose dependencies are all the previous
//! entries (plus, transitively, whatever they depended on). The "repl prefix" is
//! literally handed back to the compiler as ordinary dependency packages.

use meadow::{complete, stdlib};
use meadow_compiler::{
    ast, core, compile_unit, diagnostics, hir,
    intern::InternedString,
    lexer::tokenize,
    parser,
    source::{Source, SourceKind},
    span::{Located, Span},
    AstModule, CompiledPackage, Options,
};
use meadow_eval as eval;
use itertools::Either;
use rustyline::{
    completion::{Completer, Pair},
    error::ReadlineError,
    highlight::Highlighter,
    validate::{ValidationResult, Validator},
    Cmd, CompletionType, Config, Editor, Helper, Hinter, KeyCode, KeyEvent, Modifiers,
};
use std::borrow::Cow;

/// The rustyline helper: multi-line validation plus namespace-aware completion.
///
/// [`Session`] refreshes `names` after every entry, so a `def` or a `use` is
/// completable on the next line.
#[derive(Helper, Hinter, Default)]
struct TermValidator {
    names: complete::Names,
}

/// The prompt, in bold green — the same colour as the logo.
///
/// Two gates agree here: rustyline only asks for a highlighted prompt once it
/// has decided colour is wanted, and `yansi` emits nothing when *it* thinks
/// otherwise. Layout is measured from the raw prompt and rustyline's width
/// calculation skips escape sequences, so the colour costs no display width and
/// the cursor stays put.
impl Highlighter for TermValidator {
    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        default: bool,
    ) -> Cow<'b, str> {
        use yansi::Paint as _;
        if default {
            Cow::Owned(prompt.green().bold().to_string())
        } else {
            // Not ours — rustyline's own search prompts, say.
            Cow::Borrowed(prompt)
        }
    }
}

impl Completer for TermValidator {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let start = complete::word_start(line, pos);
        let ctx = complete::context(&line[..pos]);
        let prefix = &line[start..pos];
        let candidates = self
            .names
            .candidates(&ctx, prefix)
            .into_iter()
            .map(|c| Pair {
                display: c.clone(),
                replacement: c,
            })
            .collect();
        Ok((start, candidates))
    }
}

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

/// When to colour REPL output, in precedence order: `NO_COLOR` off, then
/// `CLICOLOR_FORCE` on, then "stdout is a terminal and `CLICOLOR` allows it".
///
/// Deliberately not yansi's `TTY_AND_COLOR`, which additionally insists *stderr*
/// is a terminal — `meadow 2> log` would come out monochrome for no good reason.
static COLOUR: yansi::Condition = yansi::Condition::from(|| {
    // Someone who sets `NO_COLOR` wants none, whatever else is set.
    if env_flag("NO_COLOR") {
        return false;
    }
    if env_flag("CLICOLOR_FORCE") {
        return true;
    }
    yansi::Condition::stdout_is_tty() && yansi::Condition::clicolor()
});

/// A `NO_COLOR`-style flag: present, non-empty, and not `"0"`.
fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

const LOGO: &str = r#"      __  ___               __
     /  |/  /__  ____ _____/ /___ _      __
    / /|_/ / _ \/ __ `/ __  / __ \ | /| / /
   / /  / /  __/ /_/ / /_/ / /_/ / |/ |/ /
  /_/  /_/\___/\__,_/\__,_/\____/|__/|__/"#;

/// The `:` commands, as `(invocation, what it does)`.
const COMMANDS: &[(&str, &str)] = &[
    (":q", "quit"),
    (":t <expr>", "type-check without evaluating"),
    (":module", "list the bindings in scope"),
    (":reset", "forget everything defined so far"),
];

/// Print the startup banner.
///
/// Unlike the prompt — which rustyline only asks to highlight when it has
/// already decided colour is wanted — this goes straight to stdout before
/// rustyline has set anything up. `yansi` makes that decision instead: it checks
/// for a TTY, honours `NO_COLOR` / `CLICOLOR`, and on Windows turns on virtual
/// terminal processing itself, emitting nothing at all when it cannot.
fn print_banner() {
    // Scoped deliberately: `Paint` blanket-impls onto every type, and its
    // deprecated `clear()` shadows `Vec::clear` at any wider scope.
    use yansi::Paint as _;

    println!();
    println!("{}", LOGO.green().bold());
    println!();
    println!("  {}", "Meadow REPL.".bold());
    println!();
    for (cmd, what) in COMMANDS {
        // Pad *before* styling: a width applies to the whole formatted value,
        // escape bytes included, so padding afterwards would misalign the column.
        println!("  {}  {}", format!("{cmd:<10}").cyan().bold(), what);
    }
    println!();
    println!(
        "  {} completes names — values, types, constructors and `use` paths,",
        "Tab".cyan().bold()
    );
    println!("  each in the namespace the cursor is actually in.");
    println!(
        "  {}: {} / {} insert a newline; an unfinished line (open bracket,",
        "Multi-line".bold(),
        "Alt+Enter".cyan(),
        "Ctrl+J".cyan()
    );
    println!("  dangling operator, trailing `\\`, `| ...` arms) keeps reading.");
    println!("  End a multi-line entry with a blank line.");
    println!();
}

pub struct Session {
    line: u32,
    /// The REPL is an edit-run loop, so it compiles under the debug profile.
    opts: Options,
    /// Compiled prior entries, oldest first — the dependency chain. The first
    /// `std_len` entries are the embedded `Std` package and survive `:reset`.
    prefix: Vec<CompiledPackage>,
    std_len: usize,
    /// Every `use` typed so far. A qualifier is activated per compilation unit,
    /// and each REPL line is its own unit, so they are replayed ahead of each new
    /// line to keep `use Std.Collections.List` in effect for the rest of the
    /// session.
    uses: Vec<ast::LDecl>,
}

impl Session {
    pub fn new() -> Self {
        let opts = Options::debug();
        let (std_pkgs, diags) = stdlib::compile_std(opts);
        if !diags.is_empty() {
            // A broken embedded prelude is a compiler bug, not a user error.
            for d in &diags {
                eprintln!("internal error compiling Std: {}: {}", d.filename, d.msg);
            }
        }
        let std_len = std_pkgs.len();
        Session {
            line: std_len as u32,
            opts,
            prefix: std_pkgs,
            std_len,
            uses: Vec::new(),
        }
    }

    pub fn run(&mut self) {
        let _ = env_logger::try_init();

        // One colour policy for the whole REPL. On Windows this also turns on
        // virtual terminal processing, which rustyline would otherwise only do
        // once the first `readline` starts — too late for the banner.
        yansi::whenever(COLOUR);

        // `List` completion prints the candidates instead of cycling silently,
        // which matters when a prefix matches a dozen prelude names.
        let config = Config::builder()
            .completion_type(CompletionType::List)
            .build();
        let mut rl: Editor<TermValidator, rustyline::history::FileHistory> =
            Editor::with_config(config).expect("failed to create editor");
        rl.set_helper(Some(TermValidator {
            names: complete::snapshot(&self.prefix, &self.uses),
        }));

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

        print_banner();

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
                            self.uses.clear();
                            println!("(reset)");
                        }
                        ":module" => self.list_module(),
                        _ if trimmed.starts_with(":t ") || trimmed.starts_with(":t\n") => {
                            self.handle(trimmed[2..].trim(), Mode::TypeOnly);
                        }
                        _ => self.handle(trimmed, Mode::Run),
                    }
                    // A new `def`, `data` or `use` should be completable now.
                    if let Some(h) = rl.helper_mut() {
                        h.names = complete::snapshot(&self.prefix, &self.uses);
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

        // Replay every `use` seen so far, so a qualifier stays active for the rest
        // of the session rather than only for the line that introduced it.
        let is_use = matches!(complete::peel_use(&decl), Some(_));
        let mut decls = self.uses.clone();
        decls.push(decl.clone());

        let module = Located::new(
            ast::Module {
                name: InternedString::from("repl"),
                decls,
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
            self.opts,
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
            // Remember a good `use` so later lines still see its qualifier.
            // Retyping the identical line replaces it; the same module under a new
            // alias is a separate entry, so both qualifiers stay live — which is
            // what the same two lines in a file would do.
            if is_use {
                if let Some(u) = complete::peel_use(&decl) {
                    let key = use_key(u);
                    self.uses
                        .retain(|d| complete::peel_use(d).map(use_key) != Some(key.clone()));
                    self.uses.push(decl);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Both colour states in one test: `yansi::whenever` is process-global, so
    /// splitting these would let them race under the default parallel runner.
    #[test]
    fn the_prompt_follows_the_colour_setting() {
        let h = TermValidator::default();

        // Tests do not run on a TTY, so ask for colour explicitly.
        yansi::whenever(yansi::Condition::ALWAYS);
        let got = h.highlight_prompt("> ", true);
        assert!(got.starts_with('\x1b'), "expected an escape sequence: {got:?}");
        assert!(got.ends_with("\x1b[0m"), "must reset: {got:?}");
        // Colour must not change the *visible* width, or the cursor drifts.
        assert_eq!(strip_ansi(&got), "> ");

        yansi::whenever(yansi::Condition::NEVER);
        assert_eq!(h.highlight_prompt("> ", true), "> ");

        // rustyline's own prompts are never ours to style.
        yansi::whenever(yansi::Condition::ALWAYS);
        assert_eq!(h.highlight_prompt("(i-search)`': ", false), "(i-search)`': ");
    }

    #[test]
    fn env_flag_follows_the_no_color_convention() {
        // Set, non-empty, not "0".
        for (val, want) in [("1", true), ("yes", true), ("", false), ("0", false)] {
            unsafe { std::env::set_var("MEADOW_TEST_FLAG", val) };
            assert_eq!(env_flag("MEADOW_TEST_FLAG"), want, "for {val:?}");
        }
        unsafe { std::env::remove_var("MEADOW_TEST_FLAG") };
        assert!(!env_flag("MEADOW_TEST_FLAG"));
    }

    /// Drop CSI sequences, so a styled string can be measured as displayed.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            // `ESC [ … <final byte in @..~>`
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        }
        out
    }
}

/// What makes two `use` decls the same line: the path and the alias, by name.
/// (`Located` compares spans too, which differ on every REPL line.)
fn use_key(u: &ast::UseDecl) -> (Vec<InternedString>, Option<InternedString>) {
    (
        u.path.iter().map(|s| *s.value()).collect(),
        u.alias.as_ref().map(|a| *a.value()),
    )
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

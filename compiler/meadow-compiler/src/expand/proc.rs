//! **Procedural macros**: an ordinary Meadow function, run while whoever calls
//! it is being compiled.
//!
//! A procedural macro is a function `[TokenTree] -> [TokenTree]`, exported like
//! anything else and named with a `!` where it is used. There is no dynamic
//! library, no ABI and no bridge: what crosses is `Std.Macro`'s type, built
//! from the call's argument and read back from the answer.
//!
//! Running one means linking a program and evaluating it, and that is not
//! something the compiler does -- it produces packages, and something else
//! turns those into a program. So the driver hands in a [`Runner`], and a
//! compile with none of them simply has no procedural macros: the REPL and the
//! editor say so rather than pretending.

use super::datum::Datum;
use meadow_intern::InternedString;
use meadow_lexer::tt;

/// Something that can run a procedural macro.
pub trait Runner {
    /// Run the function `name` exported by `package` on `input`, answering its
    /// `lookup`s from `scope`, or say what went wrong in a sentence that can be
    /// reported at the call.
    ///
    /// The trees that come back carry `at`, the call's span: they were not
    /// written anywhere, so the call is the only place to point at.
    fn run(
        &self,
        package: InternedString,
        name: InternedString,
        input: &[tt::TokenTree],
        at: meadow_span::Span,
        scope: &Scope<'_>,
    ) -> Result<Outcome, Failure>;
}

/// Why a run gave no answer: a sentence, and where it is about when the macro
/// said -- a `Fail` at the token that was wrong. `None` is the call itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub msg: String,
    pub at: Option<meadow_span::Span>,
}

impl From<String> for Failure {
    fn from(msg: String) -> Failure {
        Failure { msg, at: None }
    }
}

impl From<&str> for Failure {
    fn from(msg: &str) -> Failure {
        Failure {
            msg: msg.to_string(),
            at: None,
        }
    }
}

/// What a macro can read while it runs: the compile-time bindings visible
/// where it was called, by the names the call's module knows them by.
pub struct Scope<'a> {
    pub visible: &'a [(String, Datum)],
    /// Whether anything more can still be defined. Until it can not, a lookup
    /// of a name nothing has defined sets the run aside instead of answering.
    pub settled: bool,
}

/// How a run of a procedural macro ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// It answered: with these tokens, having defined these names, and having
    /// read these -- which its answer depends on as much as on its argument.
    Answered {
        trees: Vec<tt::TokenTree>,
        defined: Vec<(String, Datum)>,
        read: Vec<String>,
    },
    /// It asked for a name nothing has defined yet, and was abandoned. It is
    /// run again once something has.
    Waiting(String),
}

/// What a procedural macro's type has to be, and what it may not perform.
///
/// The signature is the sandbox. A macro runs during a build, so anything it
/// could learn from the world outside would make the build depend on when it
/// ran -- and a build that is not deterministic cannot be cached on what went
/// into it. The effects named here are the ones that would do that.
///
/// `Console` too, both ways: input is the world outside, and output goes
/// where the compiler's does -- which, under an editor, is the channel the
/// language server answers on, so one line printed breaks the protocol.
pub const FORBIDDEN: &[&str] = &[
    "Fs", "Process", "Random", "Time", "Thread", "Stm", "Console",
];

/// Whether `scheme` is `[TokenTree] -> [TokenTree]`, and what is wrong when it
/// is not.
pub fn signature(scheme: &meadow_infer::Scheme) -> Result<(), String> {
    use meadow_infer::Type;
    let bare = |t: &Type| -> Option<String> {
        match t {
            Type::Con(name, _) => Some(meadow_hir::spelling(&name.to_string()).to_string()),
            _ => None,
        }
    };
    let tokens = |t: &Type| -> bool {
        match t {
            Type::Con(name, args) => {
                let is_vector = meadow_hir::spelling(&name.to_string()) == "Vector";
                is_vector && args.len() == 1 && bare(&args[0]).as_deref() == Some("TokenTree")
            }
            _ => false,
        }
    };
    let Type::Fun(args, ret, eff) = &scheme.ty else {
        return Err("it is not a function".to_string());
    };
    // The answer has to be tokens, exactly. What it *takes* only has to be
    // something tokens can be passed as: a macro that ignores its argument
    // never constrains it, and one that only counts it gets `[a]`. Both are
    // called with tokens either way.
    let variable = |t: &Type| matches!(t, Type::Var(_) | Type::Bound(_));
    let takes = match &args[0] {
        t if variable(t) => true,
        Type::Con(name, es) if meadow_hir::spelling(&name.to_string()) == "Vector" => {
            es.len() == 1 && (variable(&es[0]) || bare(&es[0]).as_deref() == Some("TokenTree"))
        }
        _ => false,
    };
    if args.len() != 1 || !takes || !tokens(ret) {
        return Err("it is not `[TokenTree] -> [TokenTree]`".to_string());
    }
    // A macro is handed tokens and nothing else: a type that asks for a trait
    // of its argument -- `toTokens ts`, with `ts` left to be anything -- would
    // need that trait passed in too, and nothing passes it.
    if let Some(p) = scheme.preds.first() {
        return Err(format!(
            "its argument is left needing `{}`; say what it is: `(ts : [TokenTree])`",
            meadow_hir::spelling(&p.tr.to_string())
        ));
    }
    for name in performed(eff) {
        if FORBIDDEN.contains(&name.as_str()) {
            return Err(format!(
                "it performs `{name}`, which a macro may not: a build has to \
                 mean the same thing every time it is run"
            ));
        }
    }
    Ok(())
}

/// The effects a row mentions, by name.
fn performed(row: &meadow_infer::Type) -> Vec<String> {
    use meadow_infer::Type;
    match row {
        Type::RowExtend(label, _, rest) => {
            let mut out = vec![meadow_hir::spelling(&label.to_string()).to_string()];
            out.extend(performed(rest));
            out
        }
        _ => Vec::new(),
    }
}

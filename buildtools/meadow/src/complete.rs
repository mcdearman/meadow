//! Tab completion for the REPL.
//!
//! Meadow keeps types, constructors and values in separate namespaces, and the
//! grammar says which one a given position wants — so the completer decides
//! *what kind of name* the cursor is on before it decides which names match.
//!
//! Two things make that tractable without re-parsing:
//!
//! * Case carries meaning. A lower-case word is always a value (or a record
//!   field); an upper-case word is a type, a constructor, or a module qualifier.
//! * Type expressions appear **only** inside `data` / `record` / `effect`
//!   declarations — there are no annotations on `def` or in expressions — so a
//!   type position is recognisable from the first keyword of the entry.
//!
//! The entry up to the cursor is lexed, and [`context`] walks those tokens.

use meadow_compiler::intern::InternedString;
use meadow_compiler::lexer::{tokenize, Token};
use meadow_compiler::source::{Source, SourceKind};
use meadow_compiler::{ast, hir, CompiledPackage};
use std::collections::HashMap;

/// Types that exist without being declared anywhere (see `rename`'s tycon seed).
const BUILTIN_TYPES: &[&str] = &[
    "Int", "BigInt", "Float", "String", "Bool", "Unit", "List", "Array", "Char",
];

/// Everything the REPL currently knows how to name, split by namespace.
#[derive(Default, Clone)]
pub struct Names {
    /// Unqualified values: prelude, prims, and anything defined so far.
    pub values: Vec<String>,
    /// Type constructors — `Int`, `List`, and every `data` / `record` / `effect`.
    pub types: Vec<String>,
    /// Data constructors, from `data` variants and `record` names.
    pub ctors: Vec<String>,
    /// Every module, by full dotted path (`Std.Collections.List`), mapped to the
    /// value names it exports. Covers modules that have not been `use`d, which is
    /// what `use Std.X (…)` needs.
    pub modules: HashMap<String, Vec<String>>,
    /// Qualifiers currently *in scope*, mapped to what they reach. Keyed by the
    /// alias when there is one, so `use … as L` completes `L.`.
    pub qualified: HashMap<String, Vec<String>>,
}

/// What kind of name the cursor sits on.
#[derive(Debug, PartialEq)]
pub enum Ctx {
    /// A module path in a `use`. Carries the segments already typed.
    ModulePath(Vec<String>),
    /// Inside the `( … )` of a `use`: names the module exports.
    UseNames(Vec<String>),
    /// A type expression — only ever inside `data` / `record` / `effect`.
    Type,
    /// After `Mod.`: that module's members.
    Qualified(String),
    /// Expression or pattern position: values, constructors and qualifiers.
    Term,
    /// Somewhere no existing name makes sense — naming a *new* constructor or
    /// record field, for instance.
    Nothing,
}

/// Split `line[..pos]` into the part to keep and the partial word to complete.
///
/// The word is what gets replaced, so a qualified reference or a dotted module
/// path completes only its final segment.
pub fn word_start(line: &str, pos: usize) -> usize {
    let head = &line[..pos];
    head.len()
        - head
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '\'')
            .map(|c| c.len_utf8())
            .sum::<usize>()
}

/// Decide what the cursor is on, given the text before it.
pub fn context(head: &str) -> Ctx {
    let src = Source::new(SourceKind::Interactive, InternedString::from(head));
    let lexed = tokenize(src);
    let mut toks: Vec<Token> = lexed.tokens.into_iter().map(|t| *t.value).collect();

    // The word under the cursor is itself a token unless the text ends on a
    // separator; drop it so the rest reads as "what came before".
    let ends_in_word = head
        .chars()
        .last()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '\'');
    if ends_in_word {
        toks.pop();
    }

    // `@pub data …` and friends: an attribute is transparent, so look past it for
    // the keyword that decides the shape of the entry.
    let leads_with_attr = matches!(toks.first(), Some(Token::At));
    let head_toks: Vec<Token> = if leads_with_attr {
        toks.iter()
            .skip_while(|t| {
                !matches!(t, Token::Data | Token::Record | Token::Effect | Token::Use)
            })
            .cloned()
            .collect()
    } else {
        toks.clone()
    };

    match head_toks.first() {
        Some(Token::Use) => use_context(&head_toks),
        Some(Token::Data) | Some(Token::Record) | Some(Token::Effect) => {
            decl_context(&head_toks)
        }
        // Still choosing a keyword after `@pub`.
        None if leads_with_attr => Ctx::Nothing,
        // Term position — and *only* here does `Mod.` mean a qualified reference.
        // Inside a `use`, a dot continues the path; type expressions have no
        // qualified form at all.
        _ => match toks.as_slice() {
            [.., Token::UpperIdent(q), Token::Period] => Ctx::Qualified(q.to_string()),
            _ => Ctx::Term,
        },
    }
}

/// `use a.b.c`, `use a.b (x, y)`, `use a.b as C`.
fn use_context(toks: &[Token]) -> Ctx {
    // Inside the parenthesised name list.
    if let Some(open) = toks.iter().rposition(|t| matches!(t, Token::LParen)) {
        if !toks[open..].iter().any(|t| matches!(t, Token::RParen)) {
            return Ctx::UseNames(path_segments(&toks[1..open]));
        }
    }
    // After `as`, the user is inventing a name; nothing to suggest.
    if toks.iter().any(|t| matches!(t, Token::As)) {
        return Ctx::Nothing;
    }
    Ctx::ModulePath(path_segments(&toks[1..]))
}

/// The dotted path in `Seg . Seg . Seg`, ignoring a trailing `.`.
fn path_segments(toks: &[Token]) -> Vec<String> {
    toks.iter()
        .filter_map(|t| match t {
            Token::UpperIdent(s) | Token::LowerIdent(s) => Some(s.to_string()),
            _ => None,
        })
        .collect()
}

/// Inside `data` / `record` / `effect`, where type expressions live.
fn decl_context(toks: &[Token]) -> Ctx {
    let is_data = matches!(toks.first(), Some(Token::Data));

    // `record R = { f : <type> }` and `effect E { op : <type> }` — a type starts
    // after `:` and ends at the next `,` or `}`.
    if let Some(colon) = toks
        .iter()
        .rposition(|t| matches!(t, Token::Colon | Token::Comma | Token::LBrace | Token::RBrace))
    {
        if matches!(toks[colon], Token::Colon) {
            return Ctx::Type;
        }
        // Just after `{` or `,` in a field list: naming a field, not a type.
        if matches!(toks[colon], Token::LBrace | Token::Comma) {
            return Ctx::Nothing;
        }
    }

    if is_data {
        // `data D a = ` / `… | ` introduces a *new* constructor name; anything
        // after that constructor is a field type.
        match toks.last() {
            Some(Token::Eq) | Some(Token::Bar) => Ctx::Nothing,
            // Still on the left of `=`: the type's own name and parameters.
            _ if !toks.iter().any(|t| matches!(t, Token::Eq)) => Ctx::Nothing,
            _ => Ctx::Type,
        }
    } else {
        Ctx::Nothing
    }
}

/// The `use` inside a declaration, looking through any `@attr` wrapper.
pub fn peel_use(decl: &ast::LDecl) -> Option<&ast::UseDecl> {
    match decl.value() {
        ast::Decl::Use(u) => Some(u),
        ast::Decl::Attributed(_, inner) => peel_use(inner),
        _ => None,
    }
}

/// Everything nameable given the REPL's compiled prefix and the `use` decls in
/// effect.
///
/// Rebuilt from scratch after each entry rather than maintained incrementally:
/// the prefix is a few dozen packages, and this way it cannot drift from what the
/// compiler itself sees.
pub fn snapshot(prefix: &[CompiledPackage], uses: &[ast::LDecl]) -> Names {
    let mut n = Names::default();

    // Values: mirrors `compile_unit`'s flat-import rule — a package with no
    // `prelude_exports` contributes everything (REPL lines), one with a list
    // contributes just that list.
    for pkg in prefix {
        match &pkg.prelude_exports {
            None => n.values.extend(pkg.exports.iter().map(|e| e.name.to_string())),
            Some(flat) => n.values.extend(
                pkg.exports
                    .iter()
                    .filter(|e| e.module.is_empty() && flat.contains(&e.name))
                    .map(|e| e.name.to_string()),
            ),
        }
    }
    n.values.extend(hir::PRIMS.iter().map(|p| p.to_string()));

    // Types and constructors, from the declarations themselves.
    n.types.extend(BUILTIN_TYPES.iter().map(|t| t.to_string()));
    for pkg in prefix {
        for d in &pkg.data_decls {
            match d.value() {
                hir::Decl::Data(dd) => {
                    n.types.push(dd.name.to_string());
                    n.ctors.extend(dd.variants.iter().map(|v| v.name.to_string()));
                }
                hir::Decl::Record(rd) => {
                    n.types.push(rd.name.to_string());
                    n.ctors.push(rd.name.to_string());
                }
                hir::Decl::Effect(ed) => n.types.push(ed.name.to_string()),
                _ => {}
            }
        }
    }

    // Modules by full dotted path, with the values each exports.
    for pkg in prefix {
        for m in &pkg.modules {
            let mut segs = vec![pkg.name.to_string()];
            segs.extend(m.path.iter().map(|s| s.to_string()));
            let values: Vec<String> = pkg
                .exports
                .iter()
                .filter(|e| e.module == m.path)
                .map(|e| e.name.to_string())
                .collect();
            n.modules.entry(segs.join(".")).or_default().extend(values);
        }
    }

    // What each `use` brought into scope, resolved exactly as `apply_use` does —
    // a qualifier only from `as`, names unqualified otherwise. Completion that
    // disagreed with resolution would offer names that do not compile.
    let deps: Vec<&CompiledPackage> = prefix.iter().collect();
    for decl in uses {
        let Some(u) = peel_use(decl) else { continue };
        let segs: Vec<InternedString> = u.path.iter().map(|s| *s.value()).collect();
        if segs.is_empty() {
            continue;
        }
        let resolved =
            meadow_compiler::resolve_module(InternedString::from("repl"), &segs, &deps);

        match &u.alias {
            // `use M as C` — `C.name`, and nothing unqualified.
            Some(a) => {
                let mut vals: Vec<String> = resolved.map.keys().map(|k| k.to_string()).collect();
                vals.sort();
                n.qualified.insert(a.value().to_string(), vals);
            }
            // `use M` — every exported name, unqualified. No qualifier.
            None if u.names.is_empty() => {
                n.values.extend(resolved.map.keys().map(|k| k.to_string()));
            }
            None => {}
        }
        // `use M (a, b)` — just those, whether or not there is also an alias.
        n.values.extend(
            u.names
                .iter()
                .filter(|nm| resolved.map.contains_key(&*nm.value()))
                .map(|nm| nm.value().to_string()),
        );
    }

    // Operators are punctuation; completing them would only be noise. Applied
    // last, so it covers imported names too.
    n.values
        .retain(|v| v.chars().next().is_some_and(|c| c.is_alphabetic()));

    n
}

impl Names {
    /// Candidate names for `ctx`, filtered by `prefix`, sorted and deduplicated.
    pub fn candidates(&self, ctx: &Ctx, prefix: &str) -> Vec<String> {
        let mut out: Vec<String> = match ctx {
            Ctx::Nothing => return Vec::new(),
            Ctx::Type => self.types.clone(),
            // An upper-case word here is a constructor or a module qualifier; a
            // lower-case one is a value. The prefix tells us which.
            Ctx::Term => {
                if starts_upper(prefix) {
                    let mut v = self.ctors.clone();
                    v.extend(self.qualified.keys().cloned());
                    v
                } else {
                    self.values.clone()
                }
            }
            Ctx::Qualified(q) => self.qualified.get(q).cloned().unwrap_or_default(),
            Ctx::UseNames(path) => {
                self.modules.get(&path.join(".")).cloned().unwrap_or_default()
            }
            // One segment at a time: given `Std.Coll`, offer what sits directly
            // under `Std`. `path` is the segments already completed.
            Ctx::ModulePath(path) => self.module_segments(path),
        };
        out.retain(|n| n.starts_with(prefix));
        out.sort();
        out.dedup();
        out
    }

    /// The path segments that may directly follow `parent`.
    fn module_segments(&self, parent: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for m in self.modules.keys() {
            let segs: Vec<&str> = m.split('.').collect();
            if segs.len() > parent.len() && segs[..parent.len()] == parent[..] {
                out.push(segs[parent.len()].to_string());
            }
        }
        out
    }
}

fn starts_upper(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expression_position_is_a_term() {
        assert_eq!(context("1 + ma"), Ctx::Term);
        assert_eq!(context("def x = fo"), Ctx::Term);
        assert_eq!(context("match xs with | Co"), Ctx::Term);
    }

    #[test]
    fn use_completes_a_module_path() {
        assert_eq!(context("use "), Ctx::ModulePath(vec![]));
        assert_eq!(context("use Std"), Ctx::ModulePath(vec![]));
        assert_eq!(
            context("use Std."),
            Ctx::ModulePath(vec!["Std".to_string()])
        );
        assert_eq!(
            context("use Std.Coll"),
            Ctx::ModulePath(vec!["Std".to_string()])
        );
    }

    #[test]
    fn use_name_list_completes_members() {
        assert_eq!(
            context("use Std.Collections.List ("),
            Ctx::UseNames(vec![
                "Std".to_string(),
                "Collections".to_string(),
                "List".to_string()
            ])
        );
        assert_eq!(
            context("use Std.Collections.List (ma"),
            Ctx::UseNames(vec![
                "Std".to_string(),
                "Collections".to_string(),
                "List".to_string()
            ])
        );
    }

    #[test]
    fn an_alias_is_a_new_name_so_nothing_is_suggested() {
        assert_eq!(context("use Std.Collections.List as L"), Ctx::Nothing);
    }

    #[test]
    fn qualified_reference_looks_inside_the_module() {
        assert_eq!(context("List."), Ctx::Qualified("List".to_string()));
        assert_eq!(context("List.ma"), Ctx::Qualified("List".to_string()));
        assert_eq!(context("1 + List.le"), Ctx::Qualified("List".to_string()));
    }

    #[test]
    fn record_and_effect_fields_want_types_after_the_colon() {
        assert_eq!(context("record R = { name : St"), Ctx::Type);
        assert_eq!(context("effect E { op : In"), Ctx::Type);
        // ...but the field name itself is new.
        assert_eq!(context("record R = { na"), Ctx::Nothing);
    }

    #[test]
    fn data_variants_name_new_constructors_then_take_types() {
        assert_eq!(context("data D = "), Ctx::Nothing);
        assert_eq!(context("data D = Foo | "), Ctx::Nothing);
        assert_eq!(context("data D = Foo In"), Ctx::Type);
        // Left of `=` is the type's own name.
        assert_eq!(context("data D"), Ctx::Nothing);
    }

    #[test]
    fn attributes_are_transparent() {
        assert_eq!(context("@pub use Std."), Ctx::ModulePath(vec!["Std".to_string()]));
        assert_eq!(context("@pub record R = { x : In"), Ctx::Type);
    }

    #[test]
    fn candidates_respect_the_namespace() {
        let names = Names {
            values: vec!["map".into(), "max".into(), "min".into()],
            types: vec!["Int".into(), "List".into()],
            ctors: vec!["Just".into(), "None".into()],
            modules: HashMap::from([("Std.Collections.List".to_string(), vec!["map".to_string()])]),
            qualified: HashMap::from([("List".to_string(), vec!["map".to_string()])]),
        };
        assert_eq!(names.candidates(&Ctx::Term, "ma"), vec!["map", "max"]);
        assert_eq!(names.candidates(&Ctx::Type, "L"), vec!["List"]);
        assert_eq!(names.candidates(&Ctx::Term, "J"), vec!["Just"]);
        assert_eq!(
            names.candidates(&Ctx::Qualified("List".into()), ""),
            vec!["map"]
        );
        assert_eq!(names.candidates(&Ctx::Nothing, ""), Vec::<String>::new());
    }

    #[test]
    fn module_paths_complete_one_segment_at_a_time() {
        let names = Names {
            modules: HashMap::from([
                ("Std".to_string(), vec![]),
                ("Std.Collections".to_string(), vec![]),
                ("Std.Collections.List".to_string(), vec![]),
                ("Std.Fs".to_string(), vec![]),
            ]),
            ..Default::default()
        };
        assert_eq!(names.candidates(&Ctx::ModulePath(vec![]), "S"), vec!["Std"]);
        assert_eq!(
            names.candidates(&Ctx::ModulePath(vec!["Std".into()]), ""),
            vec!["Collections", "Fs"]
        );
        assert_eq!(
            names.candidates(
                &Ctx::ModulePath(vec!["Std".into(), "Collections".into()]),
                ""
            ),
            vec!["List"]
        );
    }

    /// A panic here would take the REPL down, so odd input must simply produce
    /// *some* context rather than blowing up.
    #[test]
    fn odd_input_does_not_panic() {
        for s in [
            "",
            "   ",
            "\n",
            "use",
            "use ",
            ".",
            "..",
            "Mod.",
            "\"unterminated",
            "data D = { ",
            "λ",
            "café",
            "x.ünïcode",
            "@",
            "@pub ",
            "1 + \u{1F600}",
        ] {
            let _ = context(s);
            let _ = word_start(s, s.len());
        }
    }

    #[test]
    fn word_start_finds_the_partial_name() {
        assert_eq!(word_start("1 + ma", 6), 4);
        assert_eq!(word_start("List.ma", 7), 5);
        assert_eq!(word_start("use Std.", 8), 8);
    }
}

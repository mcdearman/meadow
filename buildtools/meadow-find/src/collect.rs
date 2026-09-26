//! **What a compiled package declares**, as [`Decl`]s.
//!
//! The package's exports say what is public and what type each has; its
//! modules' trees say where each was written, and so what doc comment is above
//! it. Comments are thrown away by the lexer, so a doc is read back out of the
//! source text, as hover does it: the run of `--` lines directly above the
//! declaration.
//!
//! A value with a signature is documented above the signature, not above its
//! clauses. `fun f : T` and `| f x = ...` are one declaration, and the comment
//! a person wrote sits over the first line of it -- looking above the clause
//! would find the signature there instead of a comment, and say there is none.

use crate::{Decl, Kind, Location, Site};
use meadow_compiler::hir::{self, Bind, VarId};
use meadow_compiler::infer::{self, Scheme, VarKind};
use meadow_compiler::intern::InternedString;
use meadow_compiler::source::SourceKind;
use meadow_compiler::span::Span;
use meadow_compiler::{CompiledPackage, TypedModule};
use std::collections::{HashMap, HashSet};

/// What to take from a package.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Top-level bindings that are not exported, too. Right for the package
    /// somebody is working on, whose private functions are theirs to find;
    /// wrong for a dependency, whose private functions they cannot call.
    pub private: bool,
}

/// Where a value was written: which module, the span of its name, and the
/// span to look above for its doc comment.
struct Written {
    module: usize,
    name: Span,
    doc: Span,
}

/// Everything `pkg` declares that a search should find.
pub fn package(pkg: &CompiledPackage, options: Options) -> Vec<Decl> {
    let package = pkg.name.to_string();
    let mut out = Vec::new();

    // Where each value was written, and what each value is, when the tree
    // says more than its type does.
    let mut signed: HashMap<VarId, Written> = HashMap::new();
    let mut bound: HashMap<VarId, Written> = HashMap::new();
    let mut kinds: HashMap<VarId, Kind> = HashMap::new();

    for (mi, m) in pkg.modules.iter().enumerate() {
        let module = module_path(pkg, m);
        let at = |name: Span, doc: Span| Written {
            module: mi,
            name,
            doc,
        };
        for d in &m.hir.value.decls {
            match &*d.value {
                hir::Decl::Sig(ident, ..) => {
                    signed.insert(*ident.value, at(ident.span, ident.span));
                }
                hir::Decl::Bind(Bind::Fun(ident, ..)) => {
                    bound.insert(*ident.value, at(ident.span, ident.span));
                }
                hir::Decl::Bind(Bind::Pat(pat, _)) => {
                    let mut vars = Vec::new();
                    pat_idents(pat, &mut vars);
                    for (v, span) in vars {
                        bound.insert(v, at(span, span));
                    }
                }
                hir::Decl::Trait(td) => {
                    for method in &td.methods {
                        kinds.insert(*method.var.value, Kind::Method);
                        bound.insert(*method.var.value, at(method.var.span, method.var.span));
                    }
                    out.push(type_decl(
                        m,
                        &module,
                        &package,
                        &td.name,
                        td.name_span,
                        d.span,
                        Kind::Trait,
                    ));
                }
                hir::Decl::Effect(ed) => {
                    for (_, ident, _) in &ed.ops {
                        kinds.insert(*ident.value, Kind::Operation);
                        bound.insert(*ident.value, at(ident.span, ident.span));
                    }
                    out.push(type_decl(
                        m,
                        &module,
                        &package,
                        &ed.name,
                        ed.name_span,
                        d.span,
                        Kind::Effect,
                    ));
                }
                hir::Decl::Data(dd) => {
                    out.push(type_decl(
                        m,
                        &module,
                        &package,
                        &dd.name,
                        dd.name_span,
                        d.span,
                        Kind::Type,
                    ));
                    let spans: HashMap<&str, Span> = dd
                        .variants
                        .iter()
                        .map(|v| (&*v.name, v.name_span))
                        .collect();
                    out.extend(constructors(
                        pkg,
                        m,
                        &module,
                        &package,
                        &dd.name,
                        dd.params.len(),
                        |ctor| spans.get(ctor).copied().unwrap_or(dd.name_span),
                    ));
                }
                hir::Decl::Record(rd) => {
                    out.push(type_decl(
                        m,
                        &module,
                        &package,
                        &rd.name,
                        rd.name_span,
                        d.span,
                        Kind::Record,
                    ));
                    out.extend(constructors(
                        pkg,
                        m,
                        &module,
                        &package,
                        &rd.name,
                        rd.params.len(),
                        |_| rd.name_span,
                    ));
                }
                hir::Decl::Alias(ad) => out.push(type_decl(
                    m,
                    &module,
                    &package,
                    &ad.name,
                    ad.name_span,
                    d.span,
                    Kind::Alias,
                )),
                _ => {}
            }
        }
    }

    // Values. A name exported twice -- once where it is defined, once again by
    // a `@pub use` somewhere else in the package -- is one declaration, and
    // it is where it was defined.
    let prelude: HashSet<VarId> = match &pkg.prelude_exports {
        Some(flat) => pkg
            .exports
            .iter()
            .filter(|e| e.module.is_empty() && flat.contains(&e.name))
            .map(|e| e.var)
            .collect(),
        // A package with no prelude list contributes everything unqualified:
        // what a REPL line defines, say.
        None => pkg.exports.iter().map(|e| e.var).collect(),
    };
    let mut seen: HashSet<VarId> = HashSet::new();
    let mut value =
        |var: VarId, name: String, scheme: &Scheme, is_macro: bool, out: &mut Vec<Decl>| {
            if !seen.insert(var) {
                return;
            }
            // Defined in another package, and re-exported from this one: that
            // package's own index has it, where it belongs.
            let Some(w) = signed.get(&var).or_else(|| bound.get(&var)) else {
                return;
            };
            let m = &pkg.modules[w.module];
            let sig = crate::shape::Sig::of_scheme(scheme);
            let kind = match kinds.get(&var) {
                Some(k) => *k,
                None if is_macro => Kind::Macro,
                None if sig.args.is_empty() => Kind::Value,
                None => Kind::Function,
            };
            // The doc sits above the signature when there is one.
            let doc_at = signed.get(&var).map_or(w.doc, |s| s.doc);
            out.push(Decl {
                name: hir::spell_name(&name).into_owned(),
                module: module_path(pkg, m),
                package: package.clone(),
                kind,
                detail: signature(scheme),
                shape: Some(sig),
                doc: doc_above(&m.source.content, doc_at),
                prelude: prelude.contains(&var),
                location: location(m, w.name),
                site: Some(Site {
                    source: m.source,
                    span: w.name,
                }),
                var: Some(var),
            });
        };
    for e in &pkg.exports {
        value(e.var, e.name.to_string(), &e.scheme, e.is_macro, &mut out);
    }
    if options.private {
        let mut private: Vec<(&VarId, &Scheme)> = pkg.generalized.iter().collect();
        private.sort_by_key(|(v, _)| **v);
        for (var, scheme) in private {
            let Some(w) = signed.get(var).or_else(|| bound.get(var)) else {
                continue;
            };
            let m = &pkg.modules[w.module];
            let Some(name) = m
                .source
                .content
                .get(w.name.start as usize..w.name.end as usize)
            else {
                continue;
            };
            value(*var, name.to_string(), scheme, false, &mut out);
        }
    }
    out
}

/// `Std.Collections.Vector`: the package, then the module's path within it.
fn module_path(pkg: &CompiledPackage, m: &TypedModule) -> String {
    let mut segs = vec![pkg.name.to_string()];
    segs.extend(m.path.iter().map(|s| s.to_string()));
    segs.join(".")
}

/// A type, trait or effect. Its detail is how it was declared, on one line --
/// which says more about it than anything that could be reconstructed.
fn type_decl(
    m: &TypedModule,
    module: &str,
    package: &str,
    name: &str,
    name_span: Span,
    whole: Span,
    kind: Kind,
) -> Decl {
    let text = m
        .source
        .content
        .get(whole.start as usize..whole.end as usize)
        .unwrap_or("");
    Decl {
        name: bare(name).to_string(),
        module: module.to_string(),
        package: package.to_string(),
        kind,
        detail: one_line(text, 120),
        shape: None,
        doc: doc_above(&m.source.content, name_span),
        prelude: false,
        location: location(m, name_span),
        site: Some(Site {
            source: m.source,
            span: name_span,
        }),
        var: None,
    }
}

/// A type's constructors, as the functions they are: `Just : a -> Maybe a`.
/// That is what lets a search by type find them.
fn constructors(
    pkg: &CompiledPackage,
    m: &TypedModule,
    module: &str,
    package: &str,
    type_name: &str,
    params: usize,
    span_of: impl Fn(&str) -> Span,
) -> Vec<Decl> {
    // The table is keyed by the name as spelled, `Ordering`, where a
    // declaration carries its canonical one. And by interned string: looked
    // up with a `&str`, the lookup compiles and never finds anything, since
    // the two do not hash alike.
    let key = |s: &str| InternedString::from(s);
    let Some(variants) = pkg
        .variants
        .get(&key(type_name))
        .or_else(|| pkg.variants.get(&key(bare(type_name))))
    else {
        return Vec::new();
    };
    let result = infer::Type::Con(
        type_name.into(),
        (0..params as u32).map(infer::Type::Bound).collect(),
    );
    variants
        .iter()
        .map(|v| {
            let ty = v.fields.iter().rev().fold(result.clone(), |ret, field| {
                infer::Type::Fun(
                    vec![field.clone()],
                    Box::new(ret),
                    Box::new(infer::Type::RowEmpty),
                )
            });
            let scheme = Scheme {
                quant: vec![VarKind::Type; params],
                preds: Vec::new(),
                lacks: Vec::new(),
                ty,
            };
            let span = span_of(&v.name);
            Decl {
                name: bare(&v.name).to_string(),
                module: module.to_string(),
                package: package.to_string(),
                kind: Kind::Constructor,
                detail: signature(&scheme),
                shape: Some(crate::shape::Sig::of_scheme(&scheme)),
                doc: doc_above(&m.source.content, span),
                // `Just` needs no `use`: the prelude lists it unqualified.
                prelude: pkg.flat_ctors.contains(&v.name),
                location: location(m, span),
                site: Some(Site {
                    source: m.source,
                    span,
                }),
                var: None,
            }
        })
        .collect()
}

/// A scheme as a signature writes it. The compiler says `forall a b.` in
/// front of a polymorphic one, which no Meadow program ever does; a type
/// variable is quantified by being written, and saying so is noise.
fn signature(scheme: &Scheme) -> String {
    let full = scheme.to_string();
    match full
        .strip_prefix("forall ")
        .and_then(|s| s.split_once(". "))
    {
        Some((_, rest)) => rest.to_string(),
        None => full,
    }
}

/// A canonical name as a program writes it: `Std::Maybe::Maybe` is `Maybe`,
/// and the constructor `Maybe.Just` is `Just`.
fn bare(name: &str) -> &str {
    let spelled = hir::spelling(name);
    spelled.rsplit('.').next().unwrap_or(spelled)
}

/// `text` on one line, and no longer than `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let joined: Vec<&str> = text.split_whitespace().collect();
    let mut s = joined.join(" ");
    if s.chars().count() > max {
        s = s.chars().take(max - 1).collect::<String>() + "…";
    }
    s
}

/// The `--` comment directly above the line `span` starts on, without its
/// dashes, or `None` if there is none.
///
/// The line the declaration starts on is set aside first -- precisely, by its
/// line break, and not by dropping "one line": a declaration that starts in
/// column 0 has nothing before it on its line, and dropping a line then would
/// drop the last line of its doc.
pub fn doc_above(source: &str, span: Span) -> Option<String> {
    let before = source.get(..span.start as usize)?;
    let above = match before.rfind('\n') {
        Some(nl) => &before[..nl],
        None => return None,
    };
    let mut lines: Vec<&str> = Vec::new();
    // `split`, not `lines`: a blank line just above has to be seen to be
    // stepped over, and `lines` drops a trailing one.
    for line in above.split('\n').rev() {
        let t = line.trim();
        // A blank line between the comment and what it documents is
        // allowed, as hover allows it; once the comment has begun, one ends it.
        if t.is_empty() && lines.is_empty() {
            continue;
        }
        // An attribute on a line of its own sits between a doc and what it
        // documents: `-- ...` then `@test` then `fun ...`.
        if t.starts_with('@') && lines.is_empty() {
            continue;
        }
        match t.strip_prefix("--") {
            Some(rest) => lines.push(rest.strip_prefix(' ').unwrap_or(rest).trim_end()),
            None => break,
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(lines.join("\n"))
}

/// Where `span` is, for a person reading it.
fn location(m: &TypedModule, span: Span) -> Option<Location> {
    let file = match m.source.kind {
        SourceKind::File(path) => path.to_string(),
        SourceKind::Interactive => return None,
    };
    let text: &str = &m.source.content;
    let before = text.get(..span.start as usize)?;
    let line = before.matches('\n').count() as u32 + 1;
    let column = before.rfind('\n').map_or(before.chars().count(), |nl| {
        before[nl + 1..].chars().count()
    }) as u32
        + 1;
    Some(Location { file, line, column })
}

/// Every name a pattern binds, with where each was written.
fn pat_idents(pat: &hir::LPat, out: &mut Vec<(VarId, Span)>) {
    match &*pat.value {
        hir::Pat::Var(ident) => out.push((*ident.value, ident.span)),
        hir::Pat::As(ident, inner) => {
            out.push((*ident.value, ident.span));
            pat_idents(inner, out);
        }
        hir::Pat::Ann(inner, _) => pat_idents(inner, out),
        hir::Pat::Cons(_, items)
        | hir::Pat::Tuple(items)
        | hir::Pat::Array(items)
        | hir::Pat::List(items) => items.iter().for_each(|p| pat_idents(p, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::doc_above;
    use meadow_compiler::span::Span;

    fn at(src: &str, word: &str) -> Span {
        let s = src.find(word).unwrap() as u32;
        Span::new(s, s + word.len() as u32)
    }

    #[test]
    fn a_doc_is_the_comment_run_above() {
        let src = "-- one\n-- two\nfun f x = x\n";
        assert_eq!(doc_above(src, at(src, "fun")).as_deref(), Some("one\ntwo"));
        assert_eq!(doc_above(src, at(src, "f x")).as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn a_declaration_in_column_zero_keeps_its_last_doc_line() {
        let src = "-- keep me\ndef x = 1\n";
        assert_eq!(doc_above(src, at(src, "def")).as_deref(), Some("keep me"));
    }

    #[test]
    fn a_blank_line_below_the_comment_is_stepped_over_as_hover_does() {
        let src = "-- mine\n\nfun f x = x\n";
        assert_eq!(doc_above(src, at(src, "fun")).as_deref(), Some("mine"));
        // Once the comment has begun, a blank line ends it.
        let src = "-- a\n\n-- b\nfun f x = x\n";
        assert_eq!(doc_above(src, at(src, "fun")).as_deref(), Some("b"));
    }

    #[test]
    fn code_above_is_no_doc() {
        let src = "fun g = 1\nfun f x = x\n";
        assert_eq!(doc_above(src, at(src, "fun f")), None);
    }

    #[test]
    fn an_attribute_between_is_stepped_over() {
        let src = "-- a test\n@test\nfun t () = 1\n";
        assert_eq!(doc_above(src, at(src, "fun")).as_deref(), Some("a test"));
    }
}

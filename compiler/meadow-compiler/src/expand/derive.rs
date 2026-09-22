//! The derive macros the compiler has built in, for the standard library's own
//! traits: `Debug`, `Display`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, and
//! `Std.String.Parse`'s `VisualStream`.
//!
//! A derive is always a macro, as in Rust: `@derive(Show)` needs a trait
//! `Show` and a macro that writes the `impl` of it. Most are procedural macros
//! a package defines (see [`super`]); these few are the compiler's own, so that
//! the standard library can derive them for its own types, which are compiled
//! before any package that could define a macro. A procedural macro of the same
//! name is found first.
//!
//! Each writes Meadow source for the `impl`, which is parsed where the derive
//! was written.

use meadow_ast as ast;

/// The source of the `impl` the built-in derive `name` writes for `decl`, if
/// `name` is one: `Ok` of the text, or `Err` of why it cannot.
pub fn builtin(name: &str, decl: &ast::LDecl) -> Option<Result<String, String>> {
    let shape = Shape::of(decl);
    Some(match name {
        "Debug" => shape.map(|s| render(&s, "Debug", "debug")),
        "Display" => shape.map(|s| render(&s, "Display", "display")),
        "PartialEq" => shape.map(|s| partial_eq(&s)),
        "Eq" => shape.map(|s| eq(&s)),
        "PartialOrd" => shape.map(|s| ordering(&s, "PartialOrd", "partialCmp", "isEqualPartial")),
        "Ord" => shape.map(|s| ordering(&s, "Ord", "compare", "isEqualOrdering")),
        "VisualStream" => shape.map(|s| visual(&s)),
        _ => return None,
    })
}

/// What a derive needs of a declaration: its name, its type parameters, and
/// its constructors' names and fields.
struct Shape {
    name: String,
    params: Vec<String>,
    ctors: Vec<Ctor>,
    /// A `record`, whose one constructor is the type's own name.
    record: bool,
}

struct Ctor {
    name: String,
    fields: Fields,
}

enum Fields {
    Positional(usize),
    Named(Vec<String>),
}

impl Shape {
    fn of(decl: &ast::LDecl) -> Result<Shape, String> {
        match &*decl.value {
            ast::Decl::Attributed(_, inner) => Shape::of(inner),
            ast::Decl::Data(d) => Ok(Shape {
                name: d.name.value().to_string(),
                params: d.params.iter().map(|p| p.value().to_string()).collect(),
                ctors: d
                    .variants
                    .iter()
                    .map(|v| Ctor {
                        name: v.name.value().to_string(),
                        fields: match &v.fields {
                            ast::VariantFields::Positional(tys) => Fields::Positional(tys.len()),
                            ast::VariantFields::Named(fs) => Fields::Named(
                                fs.iter().map(|f| f.name.value().to_string()).collect(),
                            ),
                        },
                    })
                    .collect(),
                record: false,
            }),
            ast::Decl::Record(r) => Ok(Shape {
                name: r.name.value().to_string(),
                params: r.params.iter().map(|p| p.value().to_string()).collect(),
                ctors: vec![Ctor {
                    name: r.name.value().to_string(),
                    fields: Fields::Named(
                        r.fields
                            .iter()
                            .map(|f| f.name.value().to_string())
                            .collect(),
                    ),
                }],
                record: true,
            }),
            _ => Err("only a `data` or `record` declaration can be derived for".to_string()),
        }
    }

    /// `T a b`, parenthesized when it has parameters.
    fn head(&self) -> String {
        if self.params.is_empty() {
            self.name.clone()
        } else {
            format!("({} {})", self.name, self.params.join(" "))
        }
    }

    /// `where Tr a, Tr b`, or nothing.
    fn context(&self, tr: &str) -> String {
        if self.params.is_empty() {
            String::new()
        } else {
            let each: Vec<String> = self.params.iter().map(|p| format!("{tr} {p}")).collect();
            format!(" where {}", each.join(", "))
        }
    }
}

/// `impl Tr T { fun method v = match v with | … }`, each constructor rendered
/// as `Name(field, field)` -- as the runtime shows values -- with its fields
/// by `method`.
fn render(shape: &Shape, tr: &str, method: &str) -> String {
    let mut arms = String::new();
    for c in &shape.ctors {
        let qualified = if shape.record {
            c.name.clone()
        } else {
            format!("{}.{}", shape.name, c.name)
        };
        let (pattern, count) = match &c.fields {
            Fields::Positional(n) => {
                let vars: Vec<String> = (0..*n).map(|i| format!("f{i}")).collect();
                (format!("{qualified} {}", vars.join(" ")), *n)
            }
            Fields::Named(names) => {
                let binds: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("{n} = f{i}"))
                    .collect();
                (
                    format!("{qualified} {{ {} }}", binds.join(", ")),
                    names.len(),
                )
            }
        };
        let body = if count == 0 {
            format!("{:?}", c.name)
        } else {
            let parts: Vec<String> = (0..count).map(|i| format!("{method} f{i}")).collect();
            format!(
                "concatStrings #[{:?}, {}, \")\"]",
                format!("{}(", c.name),
                parts.join(", \", \", ")
            )
        };
        arms.push_str(&format!("    | {} -> {}\n", pattern.trim_end(), body));
    }
    if shape.ctors.is_empty() {
        arms.push_str("    | _ -> \"\"\n");
    }
    format!(
        "impl {tr} {}{} {{\n  fun {method} v =\n    match v with\n{arms}}}\n",
        shape.head(),
        shape.context(tr)
    )
}

impl Shape {
    /// Constructor `c` as a pattern, its fields bound to `{prefix}0`,
    /// `{prefix}1`, … -- or to `_` when `prefix` is empty -- and how many
    /// fields it has.
    fn pattern(&self, c: &Ctor, prefix: &str) -> (String, usize) {
        let qualified = if self.record {
            c.name.clone()
        } else {
            format!("{}.{}", self.name, c.name)
        };
        let var = |i: usize| {
            if prefix.is_empty() {
                "_".to_string()
            } else {
                format!("{prefix}{i}")
            }
        };
        match &c.fields {
            Fields::Positional(0) => (qualified, 0),
            Fields::Positional(n) => {
                let vars: Vec<String> = (0..*n).map(var).collect();
                (format!("{qualified} {}", vars.join(" ")), *n)
            }
            Fields::Named(names) => {
                let binds: Vec<String> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| format!("{n} = {}", var(i)))
                    .collect();
                (
                    format!("{qualified} {{ {} }}", binds.join(", ")),
                    names.len(),
                )
            }
        }
    }
}

/// `impl PartialEq T`: the same constructor, and each field equal.
fn partial_eq(shape: &Shape) -> String {
    let mut arms = String::new();
    for c in &shape.ctors {
        let (left, n) = shape.pattern(c, "f");
        let (right, _) = shape.pattern(c, "g");
        let body = if n == 0 {
            "True".to_string()
        } else {
            (0..n)
                .map(|i| format!("f{i} == g{i}"))
                .collect::<Vec<_>>()
                .join(" and ")
        };
        arms.push_str(&format!("    | ({left}, {right}) -> {body}\n"));
    }
    if shape.ctors.len() != 1 {
        arms.push_str("    | _ -> False\n");
    }
    format!(
        "impl PartialEq {}{} {{\n  fun (==) x y =\n    match (x, y) with\n{arms}}}\n",
        shape.head(),
        shape.context("PartialEq")
    )
}

/// `impl Eq T {}` -- `Eq` has no methods -- and, as Rust's derive does, a
/// check that every field is `Eq` too: a private function that asks it of
/// each, so that `@derive(Eq)` over a `Float` is refused.
fn eq(shape: &Shape) -> String {
    let mut arms = String::new();
    for c in &shape.ctors {
        let (pattern, n) = shape.pattern(c, "f");
        let body = if n == 0 {
            "()".to_string()
        } else {
            let mut body = format!("requireEq f{}", n - 1);
            for i in (0..n - 1).rev() {
                body = format!("let _ = requireEq f{i} in {body}");
            }
            body
        };
        arms.push_str(&format!("    | {pattern} -> {body}\n"));
    }
    if shape.ctors.is_empty() {
        arms.push_str("    | _ -> ()\n");
    }
    format!(
        "impl Eq {head}{ctx} {{\n}}\n\nfun _eqFieldsOf{name} v =\n  match v with\n{arms}",
        head = shape.head(),
        ctx = shape.context("Eq"),
        name = shape.name,
    )
}

/// `impl PartialOrd T` or `impl Ord T`: by constructor, in the order they are
/// declared, then by field, left to right -- each field compared only if the
/// ones before it were equal. `method` is the comparison, `equal` the test of
/// its answer.
fn ordering(shape: &Shape, tr: &str, method: &str, equal: &str) -> String {
    let mut arms = String::new();
    for c in &shape.ctors {
        let (left, n) = shape.pattern(c, "f");
        let (right, _) = shape.pattern(c, "g");
        let mut body = if n == 0 {
            format!("{method} 0 0")
        } else {
            format!("{method} f{} g{}", n - 1, n - 1)
        };
        for i in (0..n.saturating_sub(1)).rev() {
            body =
                format!("(let c{i} = {method} f{i} g{i} in if {equal} c{i} then {body} else c{i})");
        }
        arms.push_str(&format!("    | ({left}, {right}) -> {body}\n"));
    }
    let mut rank = String::new();
    if shape.ctors.len() != 1 {
        let ranks: Vec<String> = shape
            .ctors
            .iter()
            .enumerate()
            .map(|(i, c)| format!("| {} -> {i}", shape.pattern(c, "").0))
            .collect();
        rank = format!(
            "    let rank = \\v -> match v with {} in\n",
            ranks.join(" ")
        );
        arms.push_str(&format!("    | _ -> {method} (rank x) (rank y)\n"));
    }
    format!(
        "impl {tr} {}{} {{\n  fun {method} x y =\n{rank}    match (x, y) with\n{arms}}}\n",
        shape.head(),
        shape.context(tr)
    )
}

/// `impl VisualStream T { fun showToken _ t = display t }`: a stream whose
/// tokens read in a message as they display. It compiles exactly when the
/// stream's `Token` is `Display`.
fn visual(shape: &Shape) -> String {
    format!(
        "impl VisualStream {}{} {{\n  fun showToken _ t = display t\n}}\n",
        shape.head(),
        shape.context("Display")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(ctors: Vec<Ctor>, params: &[&str]) -> Shape {
        Shape {
            name: "T".to_string(),
            params: params.iter().map(|p| p.to_string()).collect(),
            ctors,
            record: false,
        }
    }

    #[test]
    fn an_ordering_compares_constructors_then_fields() {
        let s = shape(
            vec![
                Ctor {
                    name: "A".into(),
                    fields: Fields::Positional(2),
                },
                Ctor {
                    name: "B".into(),
                    fields: Fields::Positional(0),
                },
            ],
            &[],
        );
        assert_eq!(
            ordering(&s, "Ord", "compare", "isEqualOrdering"),
            "impl Ord T {\n  fun compare x y =\n    \
             let rank = \\v -> match v with | T.A _ _ -> 0 | T.B -> 1 in\n    \
             match (x, y) with\n    \
             | (T.A f0 f1, T.A g0 g1) -> (let c0 = compare f0 g0 in if isEqualOrdering c0 then compare f1 g1 else c0)\n    \
             | (T.B, T.B) -> compare 0 0\n    \
             | _ -> compare (rank x) (rank y)\n}\n"
        );
    }

    #[test]
    fn a_derive_renders_every_constructor() {
        let s = shape(
            vec![
                Ctor {
                    name: "None".into(),
                    fields: Fields::Positional(0),
                },
                Ctor {
                    name: "Some".into(),
                    fields: Fields::Positional(2),
                },
            ],
            &["a"],
        );
        assert_eq!(
            render(&s, "Debug", "debug"),
            "impl Debug (T a) where Debug a {\n  fun debug v =\n    match v with\n    \
             | T.None -> \"None\"\n    \
             | T.Some f0 f1 -> concatStrings #[\"Some(\", debug f0, \", \", debug f1, \")\"]\n}\n"
        );
    }
}

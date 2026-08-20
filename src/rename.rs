use crate::{
    ast, diagnostics::Diagnostic, hir::*, intern::InternedString, pipeline::InputMode,
    span::Located,
};
use itertools::{Either, Itertools};

const PRIMS: &[&str] = &[
    "print", "println", "+", "-", "*", "/", "%", "^", "==", "!=", "<", ">", "<=", ">=", "&&", "||",
    "neg", "!",
];

#[derive(Debug, Clone)]
pub struct Resolver {
    scope: Vec<(InternedString, VarId)>,
    vars: Vec<InternedString>,
    errors: Vec<Diagnostic>,
    input_mode: InputMode,
}

impl Resolver {
    pub fn new(input_mode: InputMode) -> Self {
        Resolver {
            scope: Vec::new(),
            vars: Vec::new(),
            errors: Vec::new(),
            input_mode,
        }
    }

    pub fn new_with_prelude(input_mode: InputMode) -> Resolver {
        let mut r = Resolver::new(input_mode);
        for name in PRIMS {
            r.bind(InternedString::from(*name));
        }
        r
    }

    fn mark(&self) -> usize {
        self.scope.len()
    }
    fn reset(&mut self, mark: usize) {
        self.scope.truncate(mark)
    }

    fn bind(&mut self, name: InternedString) -> VarId {
        let id = VarId(self.vars.len() as u32);
        self.vars.push(name.clone());
        self.scope.push((name, id));
        id
    }

    fn lookup(&self, name: InternedString) -> Option<VarId> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| *id)
    }

    pub fn resolve(&mut self, prog: &ast::Prog) -> (Prog, Vec<Diagnostic>) {
        let resolved_prog = match prog {
            ast::Prog::File(module) => Prog::File(self.resolve_module(module)),
            ast::Prog::Interactive(either) => match either {
                Either::Left(decl) => {
                    let resolved_decl = self.resolve_decl(decl);
                    Prog::Interactive(Either::Left(resolved_decl))
                }
                Either::Right(expr) => {
                    let resolved_expr = self.resolve_expr(expr);
                    Prog::Interactive(Either::Right(resolved_expr))
                }
            },
        };
        (resolved_prog, self.errors.clone())
    }

    fn resolve_module(&mut self, module: &ast::LModule) -> LModule {
        let resolved_decls = module
            .value()
            .decls
            .iter()
            .map(|decl| self.resolve_decl(decl))
            .collect_vec();
        Located::new(
            Module {
                name: module.value().name.clone(),
                decls: resolved_decls,
            },
            module.span,
        )
    }

    fn resolve_decl(&mut self, decl: &ast::LDecl) -> LDecl {
        match decl.value() {
            ast::Decl::Bind(bind) => {
                let resolved_bind = self.resolve_bind(bind);
                LDecl::new(Decl::Bind(resolved_bind), decl.span)
            }
        }
    }

    fn resolve_expr(&mut self, expr: &ast::LExpr) -> LExpr {
        match expr.value() {
            ast::Expr::Lit(lit) => {
                let resolved_lit = self.resolve_lit(lit);
                LExpr::new(Expr::Lit(resolved_lit), expr.span)
            }
            ast::Expr::Var(name) => {
                if let Some(id) = self.lookup(*name.value()) {
                    LExpr::new(Expr::Var(Located::new(id, name.span)), expr.span)
                } else {
                    self.report_error(
                        format!("Undefined variable: {}", name.value()),
                        Located::new(name.value().clone(), name.span),
                    );
                    LExpr::new(Expr::Error, expr.span)
                }
            }
            ast::Expr::Lam(params, body) => {
                let mark = self.mark();
                let resolved_params = params
                    .iter()
                    .map(|param| self.resolve_pat(param))
                    .collect_vec();
                let resolved_body = self.resolve_expr(body);
                self.reset(mark);
                LExpr::new(Expr::Lam(resolved_params, resolved_body), expr.span)
            }
            ast::Expr::App(func, args) => {
                let resolved_func = self.resolve_expr(func);
                let resolved_args = args.iter().map(|arg| self.resolve_expr(arg)).collect_vec();
                LExpr::new(Expr::App(resolved_func, resolved_args), expr.span)
            }
            ast::Expr::Let(binds, body) => {
                let mark = self.mark();
                let resolved_binds = binds
                    .iter()
                    .map(|bind| self.resolve_bind(bind))
                    .collect_vec();
                let resolved_body = self.resolve_expr(body);
                self.reset(mark);
                LExpr::new(Expr::Let(resolved_binds, resolved_body), expr.span)
            }
            ast::Expr::If(cond, then_branch, else_branch) => {
                let resolved_cond = self.resolve_expr(cond);
                let resolved_then = self.resolve_expr(then_branch);
                let resolved_else = self.resolve_expr(else_branch);
                LExpr::new(
                    Expr::If(resolved_cond, resolved_then, resolved_else),
                    expr.span,
                )
            }
            ast::Expr::Match(expr, arms) => {
                let resolved_expr = self.resolve_expr(expr);
                let resolved_arms = arms
                    .iter()
                    .map(|(pat, arm_expr)| {
                        let mark = self.mark();
                        let resolved_pat = self.resolve_pat(pat);
                        let resolved_arm_expr = self.resolve_expr(arm_expr);
                        self.reset(mark);
                        (resolved_pat, resolved_arm_expr)
                    })
                    .collect_vec();
                LExpr::new(Expr::Match(resolved_expr, resolved_arms), expr.span)
            }
            ast::Expr::UnOp(op, expr) => {
                let sym = InternedString::from(op.value().to_string());
                let f = self.lookup(sym).expect("prelude never truncated");
                let resolved_expr = self.resolve_expr(expr);
                LExpr::new(
                    Expr::App(
                        LExpr::new(Expr::Var(Located::new(f, op.span)), op.span),
                        vec![resolved_expr],
                    ),
                    expr.span,
                )
            }
            ast::Expr::BinOp(op, lhs, rhs) => {
                let sym = InternedString::from(op.value().to_string());
                let f = self.lookup(sym).expect("prelude never truncated");
                let l = self.resolve_expr(lhs);
                let r = self.resolve_expr(rhs);
                LExpr::new(
                    Expr::App(
                        LExpr::new(Expr::Var(Located::new(f, op.span)), op.span),
                        vec![l, r],
                    ),
                    expr.span,
                )
            }
            ast::Expr::Tuple(exprs) => {
                let resolved_exprs = exprs.iter().map(|e| self.resolve_expr(e)).collect_vec();
                LExpr::new(Expr::Tuple(resolved_exprs), expr.span)
            }
            _ => unimplemented!(
                "Expression resolution for this expression type is not implemented yet"
            ),
        }
    }

    fn resolve_bind(&mut self, bind: &ast::Bind) -> Bind {
        match bind {
            ast::Bind::Pat(pat, expr) => {
                let resolved_pat = self.resolve_pat(pat);
                let resolved_expr = self.resolve_expr(expr);
                Bind::Pat(resolved_pat, resolved_expr)
            }
            ast::Bind::Fun(name, params, body) => {
                let id = self.bind(name.value().clone());
                let resolved_params: Vec<Ident> = params
                    .iter()
                    .map(|param| {
                        let param_id = self.bind(param.value().clone());
                        Located::new(param_id, param.span)
                    })
                    .collect();
                let resolved_body = self.resolve_expr(body);
                Bind::Fun(Located::new(id, name.span), resolved_params, resolved_body)
            }
        }
    }

    fn resolve_pat(&mut self, pat: &ast::LPat) -> LPat {
        match pat.value() {
            ast::Pat::Wildcard => LPat::new(Pat::Wildcard, pat.span),
            ast::Pat::Var(name) => {
                let id = self.bind(name.value().clone());
                LPat::new(Pat::Var(Located::new(id, name.span)), pat.span)
            }
            ast::Pat::Lit(lit) => {
                let resolved_lit = self.resolve_lit(lit);
                LPat::new(Pat::Lit(resolved_lit), pat.span)
            }
            ast::Pat::As(name, subpat) => {
                let id = self.bind(name.value().clone());
                let resolved_subpat = self.resolve_pat(subpat);
                LPat::new(
                    Pat::As(Located::new(id, name.span), resolved_subpat),
                    pat.span,
                )
            }
            ast::Pat::List(pats) => {
                let resolved_pats = pats.iter().map(|p| self.resolve_pat(p)).collect();
                LPat::new(Pat::List(resolved_pats), pat.span)
            }
            ast::Pat::Unit => LPat::new(Pat::Unit, pat.span),
            _ => unimplemented!("Pattern resolution for this pattern type is not implemented yet"),
        }
    }

    fn resolve_lit(&self, lit: &ast::Lit) -> Lit {
        match lit {
            ast::Lit::Int(i) => Lit::Int(*i),
            ast::Lit::String(s) => Lit::String(*s),
        }
    }

    fn report_error(&mut self, msg: String, span: Located<impl std::fmt::Debug>) {
        let diag = Diagnostic {
            msg,
            filename: match &self.input_mode {
                InputMode::File(name) => name.to_string(),
                InputMode::Interactive => "<interactive>".into(),
            },
            label: (format!("Error at {:?}", span.value()), span.span),
            extra_labels: vec![],
        };
        self.errors.push(diag);
    }
}

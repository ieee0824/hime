//! Conservative source-level effect analysis. A candidate is not a purity proof.
use proc_macro2::Span;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use syn::{
    spanned::Spanned,
    visit::{self, Visit},
    *,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Candidate,
    Unknown,
    Impure,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub line: usize,
    pub column: usize,
    pub code: String,
    pub message: String,
    pub status: Status,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionReport {
    pub file: String,
    pub function: String,
    pub line: usize,
    pub status: Status,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip)]
    calls: Vec<(String, Span)>,
    #[serde(skip)]
    bindings: BTreeSet<String>,
    #[serde(skip)]
    module: Vec<String>,
}

fn path_name(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

struct Collector {
    file: String,
    module: Vec<String>,
    reports: Vec<FunctionReport>,
    statics: BTreeSet<String>,
}
impl Collector {
    fn analyze(
        &mut self,
        sig: &Signature,
        block: Option<&Block>,
        attrs: &[Attribute],
        name: String,
    ) {
        let mut report = FunctionReport {
            file: self.file.clone(),
            function: name,
            line: sig.span().start().line,
            status: Status::Candidate,
            diagnostics: vec![],
            calls: vec![],
            bindings: BTreeSet::new(),
            module: self.module.clone(),
        };
        let mut scanner = Scanner {
            report: &mut report,
            statics: &self.statics,
        };
        if sig.unsafety.is_some() || sig.abi.is_some() {
            scanner.add(
                sig.span(),
                Status::Unknown,
                "unsafe_or_ffi",
                "unsafe/FFI requires semantic review",
            );
        }
        if !attrs.is_empty() {
            scanner.add(
                sig.span(),
                Status::Unknown,
                "attribute",
                "attributes and conditional compilation are not expanded",
            );
        }
        if sig.asyncness.is_some() {
            scanner.add(
                sig.span(),
                Status::Unknown,
                "async_function",
                "async function returns a future with deferred execution",
            );
        }
        for input in &sig.inputs {
            scanner.visit_fn_arg(input);
        }
        if let ReturnType::Type(_, ty) = &sig.output {
            scanner.visit_type(ty);
        }
        if let Some(block) = block {
            scanner.visit_block(block);
        } else {
            scanner.add(
                sig.span(),
                Status::Unknown,
                "missing_body",
                "function has no body available for analysis",
            );
        }
        self.reports.push(report);
    }
}
impl<'ast> Visit<'ast> for Collector {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let start = self.reports.len();
        self.module.push(item.ident.to_string());
        visit::visit_item_mod(self, item);
        self.module.pop();
        if !item.attrs.is_empty() {
            for report in &mut self.reports[start..] {
                Scanner {
                    report,
                    statics: &self.statics,
                }
                .add(
                    item.span(),
                    Status::Unknown,
                    "module_attribute",
                    "enclosing module attributes are not expanded",
                );
            }
        }
    }
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let name = self
            .module
            .iter()
            .cloned()
            .chain([item.sig.ident.to_string()])
            .collect::<Vec<_>>()
            .join("::");
        self.analyze(&item.sig, Some(&item.block), &item.attrs, name);
        // Nested functions have a separate namespace and are never mistaken for module functions.
        self.module.push(format!(
            "{}@{}",
            item.sig.ident,
            item.sig.span().start().line
        ));
        self.visit_block(&item.block);
        self.module.pop();
    }
    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        self.module
            .push(format!("impl@{}", item.span().start().line));
        visit::visit_item_impl(self, item);
        self.module.pop();
    }
    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        let name = self
            .module
            .iter()
            .cloned()
            .chain([item.sig.ident.to_string()])
            .collect::<Vec<_>>()
            .join("::");
        self.analyze(&item.sig, Some(&item.block), &item.attrs, name);
    }
    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        self.module.push(item.ident.to_string());
        visit::visit_item_trait(self, item);
        self.module.pop();
    }
    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        let name = self
            .module
            .iter()
            .cloned()
            .chain([item.sig.ident.to_string()])
            .collect::<Vec<_>>()
            .join("::");
        self.analyze(&item.sig, item.default.as_ref(), &item.attrs, name);
    }
}

struct Scanner<'a> {
    report: &'a mut FunctionReport,
    statics: &'a BTreeSet<String>,
}
impl Scanner<'_> {
    fn add(&mut self, span: Span, status: Status, code: &str, message: &str) {
        self.report.status = self.report.status.max(status);
        self.report.diagnostics.push(Diagnostic {
            line: span.start().line,
            column: span.start().column + 1,
            status,
            code: code.into(),
            message: message.into(),
        });
    }
}
impl<'ast> Visit<'ast> for Scanner<'_> {
    fn visit_pat_ident(&mut self, node: &'ast PatIdent) {
        self.report.bindings.insert(node.ident.to_string());
        visit::visit_pat_ident(self, node);
    }
    fn visit_item(&mut self, _: &'ast Item) {} // Declarations are analyzed separately, not executed.
    fn visit_type_reference(&mut self, node: &'ast TypeReference) {
        if node.mutability.is_some() {
            self.add(
                node.span(),
                Status::Impure,
                "mutable_input",
                "mutable reference can expose changes to caller-owned state",
            );
        } else {
            self.add(
                node.span(),
                Status::Unknown,
                "shared_reference",
                "shared reference may contain interior mutable state",
            );
        }
        visit::visit_type_reference(self, node);
    }
    fn visit_type_ptr(&mut self, node: &'ast TypePtr) {
        self.add(
            node.span(),
            Status::Unknown,
            "raw_pointer",
            "raw pointer effects cannot be resolved",
        );
    }
    fn visit_fn_arg(&mut self, node: &'ast FnArg) {
        if let FnArg::Receiver(r) = node {
            self.add(
                r.span(),
                if r.reference.is_some() && r.mutability.is_some() {
                    Status::Impure
                } else {
                    Status::Unknown
                },
                "receiver",
                "receiver requires type and ownership analysis",
            );
        } else {
            visit::visit_fn_arg(self, node);
        }
    }
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(p) = &*node.func {
            let name = path_name(&p.path);
            if self.report.bindings.contains(&name) {
                self.add(
                    node.span(),
                    Status::Unknown,
                    "indirect_call",
                    "local binding shadows the call target",
                );
            } else if known_effect(&name) {
                self.add(
                    node.span(),
                    Status::Impure,
                    "external_effect",
                    &format!("{name} may observe or change external state"),
                );
            } else if p.qself.is_some() {
                self.add(
                    node.span(),
                    Status::Unknown,
                    "unresolved_call",
                    "qualified trait call requires type resolution",
                );
            } else {
                self.report.calls.push((name, node.span()));
            }
        } else {
            self.add(
                node.span(),
                Status::Unknown,
                "indirect_call",
                "indirect call target is unknown",
            );
            self.visit_expr(&node.func);
        }
        for arg in &node.args {
            self.visit_expr(arg);
        }
    }
    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.add(
            node.span(),
            Status::Unknown,
            "method_call",
            &format!("method {} requires type resolution", node.method),
        );
        visit::visit_expr_method_call(self, node);
    }
    fn visit_macro(&mut self, node: &'ast Macro) {
        let name = path_name(&node.path);
        let status = if [
            "print",
            "println",
            "eprint",
            "eprintln",
            "dbg",
            "panic",
            "assert",
            "assert_eq",
            "assert_ne",
            "todo",
            "unimplemented",
        ]
        .contains(&name.as_str())
            || name.starts_with("std::")
                && ["print", "println", "eprint", "eprintln", "dbg", "panic"].contains(
                    &node
                        .path
                        .segments
                        .last()
                        .unwrap()
                        .ident
                        .to_string()
                        .as_str(),
                ) {
            Status::Impure
        } else {
            Status::Unknown
        };
        self.add(
            node.span(),
            status,
            "macro",
            &format!("macro {name}! may perform effects; expansion is unavailable"),
        );
    }
    fn visit_expr_path(&mut self, node: &'ast ExprPath) {
        if node
            .path
            .segments
            .iter()
            .any(|s| self.statics.contains(&s.ident.to_string()))
        {
            self.add(
                node.span(),
                Status::Impure,
                "static_state",
                "possible access to static state",
            );
        }
    }
    fn visit_expr_unsafe(&mut self, node: &'ast ExprUnsafe) {
        self.add(
            node.span(),
            Status::Unknown,
            "unsafe_block",
            "unsafe block requires manual review",
        );
        visit::visit_expr_unsafe(self, node);
    }
    fn visit_expr_unary(&mut self, node: &'ast ExprUnary) {
        if matches!(node.op, UnOp::Deref(_)) {
            self.add(
                node.span(),
                Status::Unknown,
                "dereference",
                "dereference may access externally mutable state",
            );
        }
        visit::visit_expr_unary(self, node);
    }
    fn visit_expr_closure(&mut self, node: &'ast ExprClosure) {
        self.add(
            node.span(),
            Status::Unknown,
            "closure",
            "closure execution and captured effects require dataflow analysis",
        );
        visit::visit_expr_closure(self, node);
    }
    fn visit_expr_async(&mut self, node: &'ast ExprAsync) {
        self.add(
            node.span(),
            Status::Unknown,
            "async",
            "future execution requires dataflow analysis",
        );
        visit::visit_expr_async(self, node);
    }
    fn visit_expr_await(&mut self, node: &'ast ExprAwait) {
        self.add(
            node.span(),
            Status::Unknown,
            "await",
            "awaited future may perform effects",
        );
        visit::visit_expr_await(self, node);
    }
    fn visit_expr_reference(&mut self, node: &'ast ExprReference) {
        if node.mutability.is_some() {
            self.add(
                node.span(),
                Status::Unknown,
                "mutable_borrow",
                "mutable borrow needs alias and ownership analysis",
            );
        }
        visit::visit_expr_reference(self, node);
    }
    fn visit_attribute(&mut self, node: &'ast Attribute) {
        self.add(
            node.span(),
            Status::Unknown,
            "attribute",
            "attribute is not expanded",
        );
    }
}

fn known_effect(name: &str) -> bool {
    [
        "std::fs::",
        "std::io::",
        "std::env::",
        "std::net::",
        "std::process::",
        "std::thread::",
        "std::time::",
        "rand::",
        "getrandom::",
    ]
    .iter()
    .any(|p| name.starts_with(p))
}

/// Analyze one file, including inline modules, and propagate effects to direct local callers.
/// This does not expand macros, resolve imports/types, or prove termination.
pub fn analyze_source(file: &str, source: &str) -> syn::Result<Vec<FunctionReport>> {
    let ast = syn::parse_file(source)?;
    struct Statics(BTreeSet<String>);
    impl<'ast> Visit<'ast> for Statics {
        fn visit_item_static(&mut self, item: &'ast ItemStatic) {
            self.0.insert(item.ident.to_string());
        }
    }
    let mut statics = Statics(BTreeSet::new());
    statics.visit_file(&ast);
    let mut collector = Collector {
        file: file.into(),
        module: vec![],
        reports: vec![],
        statics: statics.0,
    };
    collector.visit_file(&ast);
    let mut reports = collector.reports;
    let mut names: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, report) in reports.iter().enumerate() {
        names.entry(report.function.clone()).or_default().push(i);
    }
    let mut edges = vec![vec![]; reports.len()];
    for (i, report) in reports.iter_mut().enumerate() {
        for (call, span) in report.calls.clone() {
            let target = resolve_call(&call, &report.module);
            match names.get(&target).filter(|v| v.len() == 1) {
                Some(indices) => edges[i].push((indices[0], span)),
                None => {
                    let mut scanner = Scanner {
                        report,
                        statics: &BTreeSet::new(),
                    };
                    scanner.add(
                        span,
                        Status::Unknown,
                        "unresolved_call",
                        &format!("call {call} cannot be resolved in this file"),
                    );
                }
            }
        }
    }
    // Recursive call graphs cannot establish totality at the source level.
    for start in 0..reports.len() {
        let mut pending: Vec<_> = edges[start].iter().map(|(target, _)| *target).collect();
        let mut seen = BTreeSet::new();
        while let Some(next) = pending.pop() {
            if next == start {
                let span = reports[start]
                    .calls
                    .first()
                    .map(|(_, span)| *span)
                    .unwrap_or_else(Span::call_site);
                let mut scanner = Scanner {
                    report: &mut reports[start],
                    statics: &BTreeSet::new(),
                };
                scanner.add(
                    span,
                    Status::Unknown,
                    "recursion",
                    "recursive call graph requires termination analysis",
                );
                break;
            }
            if seen.insert(next) {
                pending.extend(edges[next].iter().map(|(target, _)| *target));
            }
        }
    }
    loop {
        let previous: Vec<_> = reports.iter().map(|r| r.status).collect();
        for (i, calls) in edges.iter().enumerate() {
            for (target, _) in calls {
                reports[i].status = reports[i].status.max(previous[*target]);
            }
        }
        if reports.iter().map(|r| r.status).eq(previous) {
            break;
        }
    }
    for (i, calls) in edges.iter().enumerate() {
        for (target, span) in calls {
            let status = reports[*target].status;
            if status != Status::Candidate {
                let message = format!(
                    "call to {} propagates {status:?}",
                    reports[*target].function
                );
                let mut scanner = Scanner {
                    report: &mut reports[i],
                    statics: &BTreeSet::new(),
                };
                scanner.add(*span, status, "callee_effect", &message);
            }
        }
        reports[i].diagnostics.sort_by_key(|d| (d.line, d.column));
    }
    Ok(reports)
}
fn resolve_call(call: &str, module: &[String]) -> String {
    let mut parts: Vec<_> = call.split("::").map(str::to_owned).collect();
    let mut prefix = module.to_vec();
    if parts.first().is_some_and(|p| p == "crate") {
        prefix.clear();
        parts.remove(0);
    } else if parts.first().is_some_and(|p| p == "self") {
        parts.remove(0);
    } else {
        while parts.first().is_some_and(|p| p == "super") {
            prefix.pop();
            parts.remove(0);
        }
    }
    prefix.extend(parts);
    prefix.join("::")
}

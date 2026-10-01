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
    calls: Vec<Call>,
    /// Name used to resolve calls; only free and block-local functions can be call targets.
    #[serde(skip)]
    key: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct Call {
    name: String,
    span: Span,
    /// Lexical scope at the call site. Segments starting with `#` are blocks.
    scope: Vec<String>,
}

impl FunctionReport {
    fn add(&mut self, span: Span, status: Status, code: &str, message: &str) {
        self.status = self.status.max(status);
        self.diagnostics.push(Diagnostic {
            line: span.start().line,
            column: span.start().column + 1,
            status,
            code: code.into(),
            message: message.into(),
        });
    }
}

/// Scope segment for a block, unique within the file so collector and scanner agree.
fn block_id(block: &Block) -> String {
    let span = block.span();
    format!(
        "#{}:{}-{}:{}",
        span.start().line,
        span.start().column,
        span.end().line,
        span.end().column
    )
}

fn path_name(path: &Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

/// Attributes that cannot change what a function body executes.
fn is_benign(attr: &Attribute) -> bool {
    let name = path_name(attr.path());
    [
        "doc",
        "inline",
        "must_use",
        "allow",
        "warn",
        "deny",
        "forbid",
        "expect",
        "cold",
        "track_caller",
        "deprecated",
        "rustfmt::skip",
    ]
    .contains(&name.as_str())
        || name.starts_with("clippy::")
}

fn type_name(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .last()
            .map_or_else(|| "_".into(), |s| s.ident.to_string()),
        _ => "_".into(),
    }
}

/// Items declared anywhere in the file that affect how expressions are classified.
#[derive(Default)]
struct FileItems {
    statics: BTreeSet<String>,
}
impl<'ast> Visit<'ast> for FileItems {
    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        self.statics.insert(item.ident.to_string());
    }
}

/// What a resolved call path refers to.
#[derive(Clone, Copy)]
enum Target {
    Function(usize),
    /// Tuple struct and variant constructors only build a value.
    Constructor,
}

struct Collector<'a> {
    file: String,
    /// Resolution path: module names plus `#` segments for enclosing blocks.
    path: Vec<String>,
    /// Human-readable prefix for report names.
    display: Vec<String>,
    reports: Vec<FunctionReport>,
    constructors: Vec<Vec<String>>,
    items: &'a FileItems,
}
impl Collector<'_> {
    fn analyze(
        &mut self,
        sig: &Signature,
        block: Option<&Block>,
        attrs: &[Attribute],
        key: Option<Vec<String>>,
    ) {
        let mut report = FunctionReport {
            file: self.file.clone(),
            function: self
                .display
                .iter()
                .cloned()
                .chain([sig.ident.to_string()])
                .collect::<Vec<_>>()
                .join("::"),
            line: sig.span().start().line,
            status: Status::Candidate,
            diagnostics: vec![],
            calls: vec![],
            key,
        };
        let mut scanner = Scanner {
            report: &mut report,
            items: self.items,
            in_signature: true,
            scope: self.path.clone(),
            bindings: vec![BTreeSet::new()],
        };
        if sig.unsafety.is_some() || sig.abi.is_some() {
            scanner.add(
                sig.span(),
                Status::Unknown,
                "unsafe_or_ffi",
                "unsafe/FFI requires semantic review",
            );
        }
        if attrs.iter().any(|a| !is_benign(a)) {
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
            match input {
                FnArg::Receiver(r) => {
                    let explicit = r.colon_token.is_some();
                    let mutable_ref = if explicit {
                        matches!(&*r.ty, Type::Reference(t) if t.mutability.is_some())
                    } else {
                        r.reference.is_some() && r.mutability.is_some()
                    };
                    scanner.add(
                        r.span(),
                        if mutable_ref {
                            Status::Impure
                        } else {
                            Status::Unknown
                        },
                        "receiver",
                        "receiver requires type and ownership analysis",
                    );
                    if explicit {
                        // The top-level `&mut` is already covered by the receiver diagnostic.
                        match &*r.ty {
                            Type::Reference(t) => scanner.visit_type(&t.elem),
                            ty => scanner.visit_type(ty),
                        }
                    }
                }
                FnArg::Typed(t) => {
                    for attr in &t.attrs {
                        scanner.visit_attribute(attr);
                    }
                    scanner.visit_pat(&t.pat);
                    scanner.signature_type(&t.ty);
                }
            }
        }
        if let ReturnType::Type(_, ty) = &sig.output {
            scanner.signature_type(ty);
        }
        scanner.in_signature = false;
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
        if let Some(block) = block {
            // Items in the body form a block scope that only this body and its items can see.
            self.display.push(sig.ident.to_string());
            self.visit_block(block);
            self.display.pop();
        }
    }
    fn mark_enclosed(&mut self, start: usize, attrs: &[Attribute], span: Span, code: &str) {
        if attrs.iter().all(is_benign) {
            return;
        }
        for report in &mut self.reports[start..] {
            report.add(
                span,
                Status::Unknown,
                code,
                "enclosing item attributes are not expanded",
            );
        }
    }
}
impl<'ast> Visit<'ast> for Collector<'_> {
    fn visit_file(&mut self, file: &'ast File) {
        visit::visit_file(self, file);
        let span = file
            .attrs
            .iter()
            .find(|a| !is_benign(a))
            .map_or_else(Span::call_site, |a| a.span());
        self.mark_enclosed(0, &file.attrs, span, "file_attribute");
    }
    fn visit_block(&mut self, block: &'ast Block) {
        self.path.push(block_id(block));
        visit::visit_block(self, block);
        self.path.pop();
    }
    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        if matches!(item.fields, Fields::Unnamed(_)) {
            self.constructors.push(
                self.path
                    .iter()
                    .cloned()
                    .chain([item.ident.to_string()])
                    .collect(),
            );
        }
    }
    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        for variant in &item.variants {
            if matches!(variant.fields, Fields::Unnamed(_)) {
                let names = [item.ident.to_string(), variant.ident.to_string()];
                self.constructors
                    .push(self.path.iter().cloned().chain(names).collect());
            }
        }
    }
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let start = self.reports.len();
        self.path.push(item.ident.to_string());
        self.display.push(item.ident.to_string());
        visit::visit_item_mod(self, item);
        self.display.pop();
        self.path.pop();
        self.mark_enclosed(start, &item.attrs, item.span(), "module_attribute");
    }
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let key = self
            .path
            .iter()
            .cloned()
            .chain([item.sig.ident.to_string()])
            .collect();
        self.analyze(&item.sig, Some(&item.block), &item.attrs, Some(key));
    }
    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let start = self.reports.len();
        let name = match &item.trait_ {
            Some((_, path, _)) => format!(
                "<{} as {}>",
                type_name(&item.self_ty),
                path.segments
                    .last()
                    .map_or_else(|| "_".into(), |s| s.ident.to_string())
            ),
            None => type_name(&item.self_ty),
        };
        self.display.push(name);
        visit::visit_item_impl(self, item);
        self.display.pop();
        self.mark_enclosed(start, &item.attrs, item.span(), "impl_attribute");
    }
    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        // Associated functions are reached via `Self::`/type paths, which need type resolution.
        self.analyze(&item.sig, Some(&item.block), &item.attrs, None);
    }
    fn visit_item_trait(&mut self, item: &'ast ItemTrait) {
        let start = self.reports.len();
        self.display.push(item.ident.to_string());
        visit::visit_item_trait(self, item);
        self.display.pop();
        self.mark_enclosed(start, &item.attrs, item.span(), "trait_attribute");
    }
    fn visit_trait_item_fn(&mut self, item: &'ast TraitItemFn) {
        // `Trait::method(x)` dispatches to an impl, so default bodies are never call targets.
        self.analyze(&item.sig, item.default.as_ref(), &item.attrs, None);
    }
}

struct Scanner<'a> {
    report: &'a mut FunctionReport,
    items: &'a FileItems,
    in_signature: bool,
    /// Lexical scope at the current position.
    scope: Vec<String>,
    /// Local bindings per lexical scope, innermost last.
    bindings: Vec<BTreeSet<String>>,
}
impl Scanner<'_> {
    fn add(&mut self, span: Span, status: Status, code: &str, message: &str) {
        self.report.add(span, status, code, message);
    }
    fn is_bound(&self, name: &str) -> bool {
        self.bindings.iter().any(|frame| frame.contains(name))
    }
    /// Run `f` with a fresh binding frame that is dropped afterwards.
    fn with_frame(&mut self, f: impl FnOnce(&mut Self)) {
        self.bindings.push(BTreeSet::new());
        f(self);
        self.bindings.pop();
    }
    /// A top-level `&mut` parameter or return type hands caller-owned state to the function.
    fn signature_type(&mut self, ty: &Type) {
        match ty {
            Type::Reference(r) if r.mutability.is_some() => {
                self.add(
                    r.span(),
                    Status::Impure,
                    "mutable_input",
                    "mutable reference can expose changes to caller-owned state",
                );
                self.visit_type(&r.elem);
            }
            _ => self.visit_type(ty),
        }
    }
}
impl<'ast> Visit<'ast> for Scanner<'_> {
    fn visit_pat_ident(&mut self, node: &'ast PatIdent) {
        if let Some(frame) = self.bindings.last_mut() {
            frame.insert(node.ident.to_string());
        }
        visit::visit_pat_ident(self, node);
    }
    fn visit_block(&mut self, node: &'ast Block) {
        self.scope.push(block_id(node));
        self.with_frame(|s| visit::visit_block(s, node));
        self.scope.pop();
    }
    fn visit_local(&mut self, node: &'ast Local) {
        // The initializer runs before the new bindings come into scope.
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        if let Some(init) = &node.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.visit_pat(&node.pat);
    }
    fn visit_expr_let(&mut self, node: &'ast ExprLet) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&node.expr);
        self.visit_pat(&node.pat);
    }
    fn visit_arm(&mut self, node: &'ast Arm) {
        self.with_frame(|s| visit::visit_arm(s, node));
    }
    fn visit_expr_for_loop(&mut self, node: &'ast ExprForLoop) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&node.expr);
        self.with_frame(|s| {
            s.visit_pat(&node.pat);
            s.visit_block(&node.body);
        });
    }
    fn visit_item(&mut self, _: &'ast Item) {} // Declarations are analyzed separately, not executed.
    fn visit_type_reference(&mut self, node: &'ast TypeReference) {
        // Reference types in the body only annotate locals; borrows and dereferences are checked.
        if self.in_signature {
            if node.mutability.is_some() {
                self.add(
                    node.span(),
                    Status::Unknown,
                    "mutable_reference",
                    "nested mutable reference may expose caller-owned state",
                );
            } else {
                self.add(
                    node.span(),
                    Status::Unknown,
                    "shared_reference",
                    "shared reference may contain interior mutable state",
                );
            }
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
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(p) = &*node.func {
            let name = path_name(&p.path);
            self.visit_expr_path(p);
            if self.is_bound(&name) {
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
                self.report.calls.push(Call {
                    name,
                    span: node.span(),
                    scope: self.scope.clone(),
                });
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
        let segments: Vec<_> = node
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        let builtin = segments.len() == 1
            || segments.len() == 2 && ["std", "core"].contains(&segments[0].as_str());
        let status = if builtin
            && [
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
                "unreachable",
            ]
            .contains(&segments[segments.len() - 1].as_str())
        {
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
        let local = node.path.segments.len() == 1 && self.is_bound(&path_name(&node.path));
        if !local
            && node
                .path
                .segments
                .iter()
                .any(|s| self.items.statics.contains(&s.ident.to_string()))
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
        self.with_frame(|s| visit::visit_expr_closure(s, node));
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
        if !is_benign(node) {
            self.add(
                node.span(),
                Status::Unknown,
                "attribute",
                "attribute is not expanded",
            );
        }
    }
}

/// Known effectful functions. Entries ending in `::` match every item below that path.
fn known_effect(name: &str) -> bool {
    [
        "std::fs::",
        "std::io::stdin",
        "std::io::stdout",
        "std::io::stderr",
        "std::io::copy",
        "std::io::read_to_string",
        "std::env::",
        "std::net::TcpStream::",
        "std::net::TcpListener::",
        "std::net::UdpSocket::",
        "std::process::exit",
        "std::process::abort",
        "std::process::id",
        "std::process::Command::",
        "std::thread::",
        "std::time::SystemTime::now",
        "std::time::Instant::now",
        "rand::",
        "getrandom::",
    ]
    .iter()
    .any(|p| {
        if p.ends_with("::") {
            name.starts_with(p)
        } else {
            name == *p
        }
    })
}

/// Analyze one file, including inline modules, and propagate effects to direct local callers.
/// This does not expand macros, resolve imports/types, or prove termination.
pub fn analyze_source(file: &str, source: &str) -> syn::Result<Vec<FunctionReport>> {
    let ast = syn::parse_file(source)?;
    let mut items = FileItems::default();
    items.visit_file(&ast);
    let mut collector = Collector {
        file: file.into(),
        path: vec![],
        display: vec![],
        reports: vec![],
        constructors: vec![],
        items: &items,
    };
    collector.visit_file(&ast);
    let mut reports = collector.reports;
    let mut names: BTreeMap<Vec<String>, Vec<Target>> = BTreeMap::new();
    for (i, report) in reports.iter().enumerate() {
        if let Some(key) = &report.key {
            names
                .entry(key.clone())
                .or_default()
                .push(Target::Function(i));
        }
    }
    for key in collector.constructors {
        names.entry(key).or_default().push(Target::Constructor);
    }
    // Prelude constructors apply only when nothing in the file shadows them.
    let prelude = [Target::Constructor];
    let mut edges = vec![vec![]; reports.len()];
    for (i, report) in reports.iter_mut().enumerate() {
        for call in std::mem::take(&mut report.calls) {
            let target = resolve_call(&call.name, &call.scope)
                .into_iter()
                .find_map(|candidate| names.get(&candidate).map(Vec::as_slice))
                .or_else(|| {
                    ["Some", "Ok", "Err"]
                        .contains(&call.name.as_str())
                        .then_some(&prelude[..])
                });
            match target {
                Some([Target::Function(index)]) => edges[i].push((*index, call.span)),
                Some([Target::Constructor]) => {}
                _ => report.add(
                    call.span,
                    Status::Unknown,
                    "unresolved_call",
                    &format!("call {} cannot be resolved in this file", call.name),
                ),
            }
        }
    }
    // Recursive call graphs cannot establish totality at the source level.
    let reaches = |from: usize, to: usize| {
        let mut pending = vec![from];
        let mut seen = BTreeSet::new();
        while let Some(next) = pending.pop() {
            if next == to {
                return true;
            }
            if seen.insert(next) {
                pending.extend(edges[next].iter().map(|(target, _)| *target));
            }
        }
        false
    };
    for (start, calls) in edges.iter().enumerate() {
        if let Some((_, span)) = calls.iter().find(|(target, _)| reaches(*target, start)) {
            reports[start].add(
                *span,
                Status::Unknown,
                "recursion",
                "recursive call graph requires termination analysis",
            );
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
                reports[i].add(*span, status, "callee_effect", &message);
            }
        }
        reports[i].diagnostics.sort_by_key(|d| (d.line, d.column));
    }
    Ok(reports)
}

/// Drop trailing block segments to get the enclosing module.
fn module_of(scope: &[String]) -> Vec<String> {
    let end = scope
        .iter()
        .rposition(|s| !s.starts_with('#'))
        .map_or(0, |i| i + 1);
    scope[..end].to_vec()
}

/// Candidate definitions for a call, in lookup priority order.
fn resolve_call(call: &str, scope: &[String]) -> Vec<Vec<String>> {
    let parts: Vec<String> = call.split("::").map(str::to_owned).collect();
    let join =
        |prefix: &[String], rest: &[String]| prefix.iter().chain(rest).cloned().collect::<Vec<_>>();
    match parts[0].as_str() {
        "crate" => vec![parts[1..].to_vec()],
        "self" => vec![join(&module_of(scope), &parts[1..])],
        "super" => {
            let mut module = module_of(scope);
            let mut rest = &parts[..];
            while rest.first().is_some_and(|p| p == "super") {
                // `super` above the file root points into another file.
                if module.is_empty() {
                    return vec![];
                }
                module.pop();
                module = module_of(&module);
                rest = &rest[1..];
            }
            vec![join(&module, rest)]
        }
        _ => {
            // Items in enclosing blocks shadow module items.
            let module = module_of(scope);
            let mut candidates: Vec<_> = (module.len() + 1..=scope.len())
                .rev()
                .map(|end| join(&scope[..end], &parts))
                .collect();
            candidates.push(join(&module, &parts));
            candidates
        }
    }
}

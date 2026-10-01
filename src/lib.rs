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
    benign_meta(&attr.meta, false)
}

fn benign_meta(meta: &Meta, crate_level: bool) -> bool {
    let name = path_name(meta.path());
    if name == "cfg_attr" {
        // Its predicate selects attributes, not code. Inspect every possible attribute.
        return match meta {
            Meta::List(list) => list
                .parse_args_with(syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated)
                .is_ok_and(|args| {
                    args.len() >= 2 && args.iter().skip(1).all(|m| benign_meta(m, crate_level))
                }),
            _ => false,
        };
    }
    if crate_level {
        if [
            "no_std",
            "no_implicit_prelude",
            "recursion_limit",
            "type_length_limit",
            "crate_name",
            "crate_type",
            "windows_subsystem",
        ]
        .contains(&name.as_str())
        {
            return true;
        }
        if name == "feature" {
            // Documentation features are harmless; arbitrary language features are not.
            return match meta {
                Meta::List(list) => list
                    .parse_args_with(
                        syn::punctuated::Punctuated::<Path, Token![,]>::parse_terminated,
                    )
                    .is_ok_and(|features| {
                        !features.is_empty()
                            && features.iter().all(|p| {
                                ["doc_cfg", "doc_auto_cfg"].contains(&path_name(p).as_str())
                            })
                    }),
                _ => false,
            };
        }
    }
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
#[derive(Clone, PartialEq, Eq)]
enum Target {
    Function(usize),
    /// Tuple struct and variant constructors only build a value.
    Constructor,
    Namespace(Vec<String>),
    Type,
    Value,
    Import(usize),
    Unknown,
}

impl Target {
    fn in_namespace(&self, namespace: bool) -> bool {
        match self {
            Self::Namespace(_) | Self::Type => namespace,
            Self::Function(_) | Self::Constructor | Self::Value => !namespace,
            Self::Import(_) | Self::Unknown => true,
        }
    }
}

type LookupStack = BTreeSet<(Vec<String>, bool)>;

#[derive(Clone)]
struct Import {
    scope: Vec<String>,
    path: Vec<String>,
    absolute: bool,
}

#[derive(Default)]
struct Definitions {
    names: BTreeMap<Vec<String>, Vec<Target>>,
    imports: Vec<Import>,
    globs: BTreeMap<Vec<String>, Vec<usize>>,
}

struct Resolution {
    key: Vec<String>,
    targets: Vec<Target>,
}

impl Resolution {
    fn unknown(key: Vec<String>) -> Self {
        Self {
            key,
            targets: vec![Target::Unknown],
        }
    }
}

impl Definitions {
    /// Resolve imports lazily, with cycle detection for aliases and glob reexports.
    fn resolve(
        &self,
        path: &[String],
        scope: &[String],
        absolute: bool,
        namespace: bool,
        visiting: &mut LookupStack,
    ) -> Option<Resolution> {
        let first = path.first()?;
        if absolute {
            // In edition 2024, leading `::` addresses the external-crate prelude.
            return Some(Resolution::unknown(path.to_vec()));
        }
        let (prefix, rest) = match first.as_str() {
            "crate" => (vec![], &path[1..]),
            "self" => (module_of(scope), &path[1..]),
            "super" => {
                let mut module = module_of(scope);
                let mut rest = path;
                while rest.first().is_some_and(|p| p == "super") {
                    if module.is_empty() {
                        return Some(Resolution::unknown(path.to_vec()));
                    }
                    module.pop();
                    module = module_of(&module);
                    rest = &rest[1..];
                }
                (module, rest)
            }
            _ => {
                for end in (module_of(scope).len()..=scope.len()).rev() {
                    if let Some(found) =
                        self.lookup(&scope[..end], first, path.len() > 1 || namespace, visiting)
                    {
                        return Some(self.descend(found, &path[1..], namespace, visiting));
                    }
                }
                return None;
            }
        };
        let found = Resolution {
            key: prefix.clone(),
            targets: vec![Target::Namespace(prefix)],
        };
        Some(self.descend(found, rest, namespace, visiting))
    }

    fn descend(
        &self,
        mut found: Resolution,
        rest: &[String],
        namespace: bool,
        visiting: &mut LookupStack,
    ) -> Resolution {
        for (index, name) in rest.iter().enumerate() {
            let prefix = match found.targets.as_slice() {
                [Target::Namespace(prefix)] => prefix,
                _ => return Resolution::unknown(found.key),
            };
            found = match self.lookup(prefix, name, index + 1 < rest.len() || namespace, visiting) {
                Some(next) => next,
                None => return Resolution::unknown(found.key),
            };
        }
        found
    }

    fn import(
        &self,
        index: usize,
        namespace: bool,
        visiting: &mut LookupStack,
    ) -> Option<Resolution> {
        let import = &self.imports[index];
        if let Some(mut resolved) = self.resolve(
            &import.path,
            &import.scope,
            import.absolute,
            namespace,
            visiting,
        ) {
            resolved
                .targets
                .retain(|target| target.in_namespace(namespace));
            return (!resolved.targets.is_empty()).then_some(resolved);
        }
        // A known type-only import does not shadow the value namespace (and vice versa).
        let other = self.resolve(
            &import.path,
            &import.scope,
            import.absolute,
            !namespace,
            visiting,
        );
        if other.is_some_and(|r| !r.targets.contains(&Target::Unknown)) {
            return None;
        }
        Some(Resolution::unknown(import.path.clone()))
    }

    fn lookup(
        &self,
        scope: &[String],
        name: &str,
        namespace: bool,
        visiting: &mut LookupStack,
    ) -> Option<Resolution> {
        let mut key = scope.to_vec();
        key.push(name.into());
        let request = (key.clone(), namespace);
        if !visiting.insert(request.clone()) {
            return Some(Resolution::unknown(key));
        }
        let result = self.lookup_inner(scope, name, &key, namespace, visiting);
        visiting.remove(&request);
        result
    }

    fn lookup_inner(
        &self,
        scope: &[String],
        name: &str,
        key: &[String],
        namespace: bool,
        visiting: &mut LookupStack,
    ) -> Option<Resolution> {
        let mut found = vec![];
        let targets = self
            .names
            .get(key)
            .into_iter()
            .flatten()
            .filter(|target| target.in_namespace(namespace))
            .collect::<Vec<_>>();
        if !targets.is_empty() {
            // Explicit definitions and imports take priority over globs.
            for target in targets {
                match target {
                    Target::Import(index) => {
                        if let Some(imported) = self.import(*index, namespace, visiting) {
                            found.push(imported);
                        }
                    }
                    target => found.push(Resolution {
                        key: key.to_vec(),
                        targets: vec![target.clone()],
                    }),
                }
            }
        }
        if found.is_empty()
            && let Some(globs) = self.globs.get(scope)
        {
            for index in globs {
                let Some(imported) = self.import(*index, true, visiting) else {
                    return Some(Resolution::unknown(key.to_vec()));
                };
                match imported.targets.as_slice() {
                    [Target::Namespace(prefix)] => {
                        if let Some(next) = self.lookup(prefix, name, namespace, visiting) {
                            found.push(next);
                        }
                    }
                    // An unresolved glob can contain this name, so cannot be ignored.
                    _ => return Some(Resolution::unknown(key.to_vec())),
                }
            }
        }
        let mut iter = found.into_iter();
        let mut result = iter.next()?;
        for next in iter {
            if result.key != next.key {
                return Some(Resolution::unknown(key.to_vec()));
            }
            result.targets.extend(next.targets);
        }
        result
            .targets
            .retain(|target| target.in_namespace(namespace));
        if result.targets.is_empty() {
            return None;
        }
        // Reimports of the same item and cfg alternatives of a constructor agree.
        let mut unique = vec![];
        for target in result.targets {
            if !unique.contains(&target) {
                unique.push(target);
            }
        }
        result.targets = unique;
        Some(result)
    }
}

struct Collector<'a> {
    file: String,
    /// Resolution path: module names plus `#` segments for enclosing blocks.
    path: Vec<String>,
    /// Human-readable prefix for report names.
    display: Vec<String>,
    reports: Vec<FunctionReport>,
    definitions: Definitions,
    items: &'a FileItems,
}
impl Collector<'_> {
    fn define(&mut self, name: String, target: Target) {
        let mut key = self.path.clone();
        key.push(name);
        self.definitions.names.entry(key).or_default().push(target);
    }
    fn collect_use(&mut self, tree: &UseTree, prefix: &[String], absolute: bool) {
        let (name, path) = match tree {
            UseTree::Path(p) => {
                let mut path = prefix.to_vec();
                path.push(p.ident.to_string());
                self.collect_use(&p.tree, &path, absolute);
                return;
            }
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.collect_use(tree, prefix, absolute);
                }
                return;
            }
            UseTree::Name(n) => {
                if n.ident == "self" {
                    (prefix.last().cloned(), prefix.to_vec())
                } else {
                    let mut path = prefix.to_vec();
                    path.push(n.ident.to_string());
                    (Some(n.ident.to_string()), path)
                }
            }
            UseTree::Rename(n) => {
                if n.rename == "_" {
                    return;
                }
                let mut path = prefix.to_vec();
                if n.ident != "self" {
                    path.push(n.ident.to_string());
                }
                (Some(n.rename.to_string()), path)
            }
            UseTree::Glob(_) => (None, prefix.to_vec()),
        };
        let index = self.definitions.imports.len();
        self.definitions.imports.push(Import {
            scope: self.path.clone(),
            path,
            absolute,
        });
        if let Some(name) = name {
            self.define(name, Target::Import(index));
        } else {
            self.definitions
                .globs
                .entry(self.path.clone())
                .or_default()
                .push(index);
        }
    }
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
        if let Some(attr) = file.attrs.iter().find(|a| !benign_meta(&a.meta, true)) {
            for report in &mut self.reports {
                report.add(
                    attr.span(),
                    Status::Unknown,
                    "file_attribute",
                    "enclosing item attributes are not expanded",
                );
            }
        }
    }
    fn visit_block(&mut self, block: &'ast Block) {
        self.path.push(block_id(block));
        visit::visit_block(self, block);
        self.path.pop();
    }
    fn visit_item_struct(&mut self, item: &'ast ItemStruct) {
        self.define(item.ident.to_string(), Target::Type);
        if matches!(item.fields, Fields::Unnamed(_)) {
            self.define(item.ident.to_string(), Target::Constructor);
        }
        visit::visit_item_struct(self, item);
    }
    fn visit_item_enum(&mut self, item: &'ast ItemEnum) {
        let mut namespace = self.path.clone();
        namespace.push(item.ident.to_string());
        self.define(item.ident.to_string(), Target::Namespace(namespace.clone()));
        for variant in &item.variants {
            let mut key = namespace.clone();
            key.push(variant.ident.to_string());
            self.definitions.names.entry(key).or_default().push(
                if matches!(variant.fields, Fields::Unnamed(_)) {
                    Target::Constructor
                } else {
                    Target::Unknown
                },
            );
        }
        visit::visit_item_enum(self, item);
    }
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        self.collect_use(&item.tree, &[], item.leading_colon.is_some());
    }
    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        self.define(item.ident.to_string(), Target::Value);
        visit::visit_item_const(self, item);
    }
    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        self.define(item.ident.to_string(), Target::Value);
        visit::visit_item_static(self, item);
    }
    fn visit_item_type(&mut self, item: &'ast ItemType) {
        self.define(item.ident.to_string(), Target::Type);
        visit::visit_item_type(self, item);
    }
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        let start = self.reports.len();
        let mut namespace = self.path.clone();
        namespace.push(item.ident.to_string());
        self.define(
            item.ident.to_string(),
            if item.content.is_some() {
                Target::Namespace(namespace)
            } else {
                Target::Unknown
            },
        );
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
        self.define(item.ident.to_string(), Target::Type);
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
    fn visit_expr_if(&mut self, node: &'ast ExprIf) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        self.with_frame(|s| {
            s.visit_expr(&node.cond);
            s.visit_block(&node.then_branch);
        });
        if let Some((_, branch)) = &node.else_branch {
            self.visit_expr(branch);
        }
    }
    fn visit_expr_while(&mut self, node: &'ast ExprWhile) {
        for attr in &node.attrs {
            self.visit_attribute(attr);
        }
        self.with_frame(|s| {
            s.visit_expr(&node.cond);
            s.visit_block(&node.body);
        });
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
        definitions: Definitions::default(),
        items: &items,
    };
    collector.visit_file(&ast);
    let mut reports = collector.reports;
    let mut definitions = collector.definitions;
    for (i, report) in reports.iter().enumerate() {
        if let Some(key) = &report.key {
            definitions
                .names
                .entry(key.clone())
                .or_default()
                .push(Target::Function(i));
        }
    }
    // Prelude constructors apply only when nothing in the file shadows them.
    let prelude = [Target::Constructor];
    let mut edges = vec![vec![]; reports.len()];
    for (i, report) in reports.iter_mut().enumerate() {
        for call in std::mem::take(&mut report.calls) {
            let path = call.name.split("::").map(str::to_owned).collect::<Vec<_>>();
            let resolved =
                definitions.resolve(&path, &call.scope, false, false, &mut BTreeSet::new());
            let target = resolved.as_ref().map(|r| r.targets.as_slice()).or_else(|| {
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

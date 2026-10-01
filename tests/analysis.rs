use hime::{Status, analyze_source};
fn statuses(source: &str) -> Vec<Status> {
    analyze_source("test.rs", source)
        .unwrap()
        .into_iter()
        .map(|r| r.status)
        .collect()
}
#[test]
fn values_and_local_mutation_are_candidates() {
    assert_eq!(
        statuses("fn f(x: i32) -> i32 { let mut y = x; y += 1; y }"),
        [Status::Candidate]
    );
}
#[test]
fn effects_propagate_through_multiple_callers() {
    assert_eq!(
        statuses("fn a() { b(); } fn b() { c(); } fn c() { println!(\"x\"); }"),
        [Status::Impure; 3]
    );
}
#[test]
fn unresolved_calls_propagate() {
    assert_eq!(
        statuses("fn a() { b(); } fn b() { foreign(); }"),
        [Status::Unknown; 2]
    );
}
#[test]
fn modules_do_not_conflate_same_names() {
    assert_eq!(
        statuses(
            "mod a { fn f() {} fn g() { f(); } } mod b { fn f() { println!(\"x\"); } fn g() { f(); } }"
        ),
        [
            Status::Candidate,
            Status::Candidate,
            Status::Impure,
            Status::Impure
        ]
    );
}
#[test]
fn qualified_local_calls_resolve() {
    assert_eq!(
        statuses("mod a { pub fn f() { std::env::var(\"X\"); } } fn g() { crate::a::f(); }"),
        [Status::Impure; 2]
    );
}
#[test]
fn methods_macros_and_unsafe_are_unknown() {
    assert_eq!(
        statuses("fn a() { x.len(); } fn b() { custom!(); } unsafe fn c() {}"),
        [Status::Unknown; 3]
    );
}
#[test]
fn mutable_and_shared_inputs() {
    assert_eq!(
        statuses("fn a(x: &mut i32) { *x = 1; } fn b(x: &i32) -> i32 { *x }"),
        [Status::Impure, Status::Unknown]
    );
}
#[test]
fn static_access_is_detected() {
    assert_eq!(
        statuses("static X: i32 = 1; fn a() -> i32 { X }"),
        [Status::Impure]
    );
}
#[test]
fn nested_body_is_not_executed_by_outer_function() {
    assert_eq!(
        statuses("fn a() { fn b() { println!(\"x\"); } }"),
        [Status::Candidate, Status::Impure]
    );
}
#[test]
fn parse_errors_are_reported() {
    assert!(analyze_source("bad.rs", "fn {").is_err());
}

#[test]
fn shadowed_function_is_not_treated_as_local_call() {
    assert_eq!(
        statuses("fn pure() {} fn a(pure: fn()) { pure(); }"),
        [Status::Candidate, Status::Unknown]
    );
}
#[test]
fn recursive_cycles_are_unknown_and_propagate() {
    assert_eq!(
        statuses("fn a() { b(); } fn b() { a(); } fn c() { a(); }"),
        [Status::Unknown; 3]
    );
}

fn report(source: &str, function: &str) -> hime::FunctionReport {
    analyze_source("test.rs", source)
        .unwrap()
        .into_iter()
        .find(|r| r.function == function)
        .unwrap_or_else(|| panic!("{function} not reported"))
}
fn status_of(source: &str, function: &str) -> Status {
    report(source, function).status
}

// Issue #1: calls must resolve the way rustc does, or stay unresolved.
#[test]
fn plain_call_in_impl_targets_module_function() {
    let src = "fn helper() { println!(\"x\"); } struct S; impl S { fn helper() {} fn run() { helper(); } }";
    assert_eq!(status_of(src, "S::run"), Status::Impure);
}
#[test]
fn outer_function_calls_its_nested_function() {
    let src = "fn b() {} fn a() { fn b() { println!(\"x\"); } b(); }";
    assert_eq!(status_of(src, "a"), Status::Impure);
}
#[test]
fn super_above_file_root_is_unresolved() {
    assert_eq!(
        status_of("fn f() {} fn g() { super::f(); }", "g"),
        Status::Unknown
    );
}
#[test]
fn super_in_impl_skips_only_the_module() {
    let src = "fn f() { println!(\"x\"); } mod m { fn f() {} struct S; impl S { fn g() { super::f(); } } }";
    assert_eq!(status_of(src, "m::S::g"), Status::Impure);
}
#[test]
fn self_in_impl_targets_module() {
    let src = "fn f() { println!(\"x\"); } struct S; impl S { fn f() {} fn g() { self::f(); } }";
    assert_eq!(status_of(src, "S::g"), Status::Impure);
}
#[test]
fn trait_path_call_is_not_resolved_to_default_body() {
    let src = "trait T: Sized { fn m(x: Self) -> i32 { 0 } } struct S; impl T for S { fn m(x: Self) -> i32 { println!(\"x\"); 1 } } fn g(s: S) -> i32 { T::m(s) }";
    assert_eq!(status_of(src, "g"), Status::Unknown);
}
#[test]
fn plain_call_in_trait_default_targets_module() {
    let src = "fn h() { println!(\"x\"); } trait T { fn h() {} fn d() { h(); } }";
    assert_eq!(status_of(src, "T::d"), Status::Impure);
}
#[test]
fn super_in_nested_function_skips_function_scopes() {
    let src = "fn c() { println!(\"x\"); } mod m { fn c() {} fn a() { fn b() { super::c(); } } }";
    assert_eq!(status_of(src, "m::a::b"), Status::Impure);
}
#[test]
fn explicit_mutable_self_is_impure() {
    let src = "struct S { v: i32 } impl S { fn m(self: &mut Self) { self.v = 1; } }";
    assert_eq!(status_of(src, "S::m"), Status::Impure);
}
#[test]
fn nested_functions_see_sibling_block_items() {
    let src = "fn a() { fn b() { println!(\"x\"); } fn c() { b(); } }";
    assert_eq!(status_of(src, "a::c"), Status::Impure);
}

// Issue #2: benign constructs must not be reported.
#[test]
fn benign_attributes_and_constructors_are_candidates() {
    assert_eq!(
        statuses(
            "/// doc\n#[inline] #[must_use] fn a(x: i32) -> Option<i32> { Some(x) }
             struct P(i32); enum E { V(i32) } fn b() -> (P, E, Result<i32, ()>) { (P(1), E::V(2), Ok(3)) }"
        ),
        [Status::Candidate; 2]
    );
}
#[test]
fn std_value_constructors_are_not_effects() {
    assert_eq!(
        statuses(
            "fn a() { let _ = std::time::Duration::from_secs(1); } fn b() { let _ = std::io::Error::other(\"x\"); }"
        ),
        [Status::Unknown; 2]
    );
    assert_eq!(
        statuses("fn a() { let _ = std::time::Instant::now(); }"),
        [Status::Impure]
    );
}
#[test]
fn only_top_level_mutable_parameters_are_impure() {
    assert_eq!(
        statuses(
            "fn a() -> i32 { let mut v = 0; let r: &mut i32 = &mut v; v } fn b(g: impl Fn(&mut i32)) {}"
        ),
        [Status::Unknown, Status::Unknown]
    );
}
#[test]
fn unreachable_is_treated_like_panic() {
    assert_eq!(
        statuses("fn a() { unreachable!() } fn b() { core::panic!() }"),
        [Status::Impure; 2]
    );
}

// Issue #3: coverage and diagnostics.
#[test]
fn functions_nested_in_methods_are_reported() {
    let src = "struct S; impl S { fn m() { fn inner() { println!(\"x\"); } inner(); } }";
    assert_eq!(status_of(src, "S::m::inner"), Status::Impure);
    assert_eq!(status_of(src, "S::m"), Status::Impure);
}
#[test]
fn impl_and_trait_attributes_are_unknown() {
    assert_eq!(
        statuses("struct S; #[cfg(x)] impl S { fn a() {} } #[async_trait] trait T { fn b() {} }"),
        [Status::Unknown; 2]
    );
}
#[test]
fn recursion_points_to_recursive_call() {
    let r = report("fn leaf() {}\nfn a() {\n    leaf();\n    a();\n}", "a");
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == "recursion")
        .unwrap();
    assert_eq!(d.line, 4);
}
#[test]
fn trait_impl_methods_are_named_by_type() {
    let names: Vec<_> = analyze_source(
        "t.rs",
        "struct S; trait T { fn m(); } impl T for S { fn m() {} }",
    )
    .unwrap()
    .into_iter()
    .map(|r| r.function)
    .collect();
    assert_eq!(names, ["T::m", "<S as T>::m"]);
}
fn named(source: &str) -> Vec<(String, Status)> {
    analyze_source("test.rs", source)
        .unwrap()
        .into_iter()
        .map(|r| (r.function, r.status))
        .collect()
}
#[test]
fn block_local_functions_do_not_leak_out_of_their_block() {
    let reports = named(
        "fn helper() { println!(\"e\"); } pub fn run() { { fn helper() {} helper(); } helper(); }",
    );
    assert_eq!(
        reports,
        [
            ("helper".into(), Status::Impure),
            ("run".into(), Status::Impure),
            ("run::helper".into(), Status::Candidate),
        ]
    );
}
#[test]
fn block_local_functions_shadow_inside_their_block() {
    assert_eq!(
        statuses("fn helper() { println!(\"e\"); } pub fn run() { { fn helper() {} helper(); } }"),
        [Status::Impure, Status::Candidate, Status::Candidate]
    );
}
#[test]
fn constructors_in_other_modules_do_not_hide_functions() {
    assert_eq!(
        statuses(
            "mod values { pub struct helper(pub i32); } fn helper(_: i32) { println!(\"e\"); } pub fn run() { helper(1); }"
        ),
        [Status::Impure, Status::Impure]
    );
}
#[test]
fn constructors_resolve_by_scope() {
    assert_eq!(
        statuses(
            "struct W(i32); enum E { V(i32) } mod m { pub struct P(pub i32); } fn run() { W(1); E::V(1); m::P(1); Some(1); Ok::<i32, ()>(1); }"
        ),
        [Status::Candidate]
    );
}
#[test]
fn local_functions_shadow_prelude_constructors() {
    assert_eq!(
        statuses("fn Some(x: i32) -> i32 { println!(\"e\"); x } fn run() { Some(1); }"),
        [Status::Impure, Status::Impure]
    );
}
#[test]
fn let_initializer_is_resolved_before_its_binding() {
    assert_eq!(
        statuses("fn helper() { println!(\"e\"); } pub fn run() { let helper = helper(); }"),
        [Status::Impure, Status::Impure]
    );
}
#[test]
fn bindings_end_with_their_block() {
    assert_eq!(
        statuses(
            "fn helper() { println!(\"e\"); } pub fn run() { { let helper: fn() = || {}; helper(); } helper(); }"
        ),
        [Status::Impure, Status::Impure]
    );
}
#[test]
fn bindings_shadow_calls_inside_their_block() {
    let reports = analyze_source(
        "test.rs",
        "fn helper() {} pub fn run(helper: fn()) { helper(); }",
    )
    .unwrap();
    assert!(
        reports[1]
            .diagnostics
            .iter()
            .any(|d| d.code == "indirect_call")
    );
}
#[test]
fn file_attributes_apply_to_functions() {
    assert_eq!(
        statuses("#![cfg(any())]\npub fn run() {}"),
        [Status::Unknown]
    );
    assert_eq!(
        statuses("#![allow(dead_code)]\n//! doc\npub fn run() {}"),
        [Status::Candidate]
    );
}
#[test]
fn static_function_pointer_calls_access_static_state() {
    let reports = analyze_source(
        "test.rs",
        "static CALLBACK: fn() = effect; fn effect() { println!(\"e\"); } pub fn run() { CALLBACK(); }",
    )
    .unwrap();
    assert_eq!(reports[1].status, Status::Impure);
    assert!(
        reports[1]
            .diagnostics
            .iter()
            .any(|d| d.code == "static_state")
    );
}

// Issues #11–#15: regressions introduced by scope-aware collection.
#[test]
fn conditional_bindings_end_before_else_and_after_if_or_while() {
    let source = r#"
        fn helper() { println!("effect"); }
        fn after_if(o: Option<fn()>) {
            if let Some(helper) = o { helper(); }
            helper();
        }
        fn in_else(o: Option<fn()>) {
            if let Some(helper) = o { helper(); } else { helper(); }
        }
        fn after_while(o: Option<fn()>) {
            while let Some(helper) = o { helper(); break; }
            helper();
        }
        fn chain(o: Option<fn()>) {
            if let Some(helper) = o && let () = helper() { helper(); }
            helper();
        }
    "#;
    assert_eq!(statuses(source), [Status::Impure; 5]);
    for name in ["after_if", "in_else", "after_while", "chain"] {
        assert!(
            report(source, name)
                .diagnostics
                .iter()
                .any(|d| d.code == "indirect_call")
        );
    }
}

#[test]
fn local_imports_resolve_constructors_functions_and_module_aliases() {
    let source = r#"
        mod m {
            pub struct W(pub i32);
            pub enum E { V(i32) }
            pub fn effect() { println!("effect"); }
        }
        use m::{self as alias, W, E::V, effect as g};
        fn constructors() { W(1); V(2); alias::W(3); }
        fn caller() { g(); }
    "#;
    assert_eq!(status_of(source, "constructors"), Status::Candidate);
    assert_eq!(status_of(source, "caller"), Status::Impure);
}

#[test]
fn glob_imports_resolve_variants_and_reexports() {
    let source = r#"
        mod m {
            pub struct W(pub i32);
            pub enum E { V(i32) }
            pub fn effect() { println!("effect"); }
        }
        mod exports { pub use crate::m::*; }
        use exports::*;
        use m::E::*;
        fn constructors() { W(1); V(2); }
        fn caller() { effect(); }
    "#;
    assert_eq!(status_of(source, "constructors"), Status::Candidate);
    assert_eq!(status_of(source, "caller"), Status::Impure);
}

#[test]
fn imports_obey_blocks_and_relative_paths() {
    let source = r#"
        mod m { pub fn helper() { println!("effect"); } pub struct W(pub i32); }
        mod n {
            use super::m::W as P;
            fn constructor() { P(1); }
            fn effect() { use super::m::helper; helper(); }
        }
        fn helper() {}
        fn scoped() { { use m::helper; helper(); } }
        fn outside() { { use m::helper; } helper(); }
    "#;
    assert_eq!(status_of(source, "n::constructor"), Status::Candidate);
    assert_eq!(status_of(source, "n::effect"), Status::Impure);
    assert_eq!(status_of(source, "scoped"), Status::Impure);
    assert_eq!(status_of(source, "outside"), Status::Candidate);
}

#[test]
fn unresolved_and_ambiguous_imports_do_not_fall_back_to_pure_targets() {
    for source in [
        "use external::Some; fn f() { Some(1); }",
        "use external::*; fn f() { Some(1); }",
        "mod a { pub struct W(pub i32); } mod b { pub struct W(pub i32); } use a::*; use b::*; fn f() { W(1); }",
        "use a as b; use b as a; fn f() { a(); }",
        "fn helper() {} fn f() { use external::helper; helper(); }",
        "mod m; use m::W; fn f() { W(1); }",
    ] {
        assert_eq!(status_of(source, "f"), Status::Unknown, "{source}");
    }
}

#[test]
fn explicit_items_shadow_globs_and_aliases_can_chain() {
    let source = r#"
        mod m { pub fn helper() { println!("effect"); } pub struct W(pub i32); }
        use m::*;
        use m::W as A;
        use A as B;
        fn helper() {}
        fn f() { helper(); B(1); }
    "#;
    assert_eq!(status_of(source, "f"), Status::Candidate);
}

#[test]
fn imports_keep_type_and_value_namespaces_separate() {
    let source = r#"
        mod m {
            pub enum E { V(i32) }
            pub fn E() { println!("effect"); }
            pub struct T { pub x: i32 }
            pub fn T() {}
        }
        use m::{E, T};
        fn value() { E(); }
        fn constructor() { E::V(1); T(); }
        fn local() { mod E {} E(); }
    "#;
    assert_eq!(status_of(source, "value"), Status::Impure);
    assert_eq!(status_of(source, "constructor"), Status::Candidate);
    assert_eq!(status_of(source, "local"), Status::Impure);
}

#[test]
fn repeated_glob_reexports_of_the_same_item_are_not_ambiguous() {
    let source = r#"
        mod m { pub struct W(pub i32); }
        mod a { pub use crate::m::*; }
        mod b { pub use crate::m::*; }
        use a::*; use b::*;
        fn f() { W(1); }
    "#;
    assert_eq!(status_of(source, "f"), Status::Candidate);
}

#[test]
fn functions_inside_discriminants_and_field_types_are_collected() {
    assert_eq!(
        named(
            "pub enum E { A = { const fn n() -> isize { assert!(1 > 0); 1 } n() } } pub struct S([u8; { const fn m() -> usize { assert!(1 > 0); 3 } m() }]);"
        ),
        [("n".into(), Status::Impure), ("m".into(), Status::Impure)]
    );
}

#[test]
fn benign_crate_attributes_do_not_change_function_status() {
    for attrs in [
        "#![no_std] #![recursion_limit = \"256\"] #![cfg_attr(docsrs, feature(doc_cfg))]",
        "#![no_implicit_prelude] #![type_length_limit = \"100000\"] #![crate_name = \"test\"] #![crate_type = \"lib\"] #![windows_subsystem = \"windows\"]",
        "#![cfg_attr(not(feature = \"std\"), no_std)]",
        "#![cfg_attr(docsrs, cfg_attr(nightly, feature(doc_cfg)), allow(dead_code))]",
    ] {
        assert_eq!(
            statuses(&format!(
                "{attrs} fn add(a: i32, b: i32) -> i32 {{ a + b }}"
            )),
            [Status::Candidate],
            "{attrs}"
        );
    }
    for attrs in [
        "#![cfg(any())]",
        "#![cfg_attr(feature = \"x\", cfg(any()))]",
        "#![feature(specialization)]",
        "#![cfg_attr(docsrs, feature(doc_cfg, specialization))]",
        "#![custom]",
    ] {
        assert_eq!(
            statuses(&format!("{attrs} fn f() {{}}")),
            [Status::Unknown],
            "{attrs}"
        );
    }
}

#[test]
fn cfg_alternative_constructors_are_candidates_but_functions_stay_ambiguous() {
    assert_eq!(
        statuses(
            "#[cfg(unix)] struct W(i32); #[cfg(not(unix))] struct W(i64); #[cfg(unix)] enum H { Fd(i32) } #[cfg(not(unix))] enum H { Fd(i64) } fn run() { W(1); H::Fd(1); }"
        ),
        [Status::Candidate]
    );
    assert_eq!(
        status_of(
            "#[cfg(unix)] fn target() {} #[cfg(not(unix))] fn target() {} fn run() { target(); }",
            "run"
        ),
        Status::Unknown
    );
    assert_eq!(
        status_of(
            "#[cfg(unix)] struct W(i32); #[cfg(not(unix))] fn W(_: i32) {} fn run() { W(1); }",
            "run"
        ),
        Status::Unknown
    );
}

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

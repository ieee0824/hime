fn square(x: i64) -> i64 { x * x }
fn local_mutation(x: i64) -> i64 { let mut n = x; n += 1; n }
fn output() { println!("hello"); }
fn indirect() { output(); }
fn clock() { let _ = std::time::SystemTime::now(); }
fn mutate(x: &mut i64) { *x += 1; }
fn unresolved() { external_library::calculate(); }

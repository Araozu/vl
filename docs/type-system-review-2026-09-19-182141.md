# Type system implementation review

Reviewed: 2026-09-19 18:21:41 UTC-05:00 (America/Lima)

## Findings

1. [`crates/vl-typecheck/src/lib.rs:391`](../crates/vl-typecheck/src/lib.rs#L391) — 🔴 **Bug:** Any value-return satisfies a non-void function, even when reachable paths fall through; `if (x) { return 1; }` checks clean and lowers the other path to a dummy zero. Require every reachable path to return.

2. [`crates/vl-typecheck/src/lib.rs:1045`](../crates/vl-typecheck/src/lib.rs#L1045) — 🔴 **Bug:** Type-expanding recursion such as `grow[T](x) { grow([x]); }` creates infinitely many instances; the compiler timed out. Reject polymorphic recursion or enforce an instance/depth budget with a diagnostic.

3. [`crates/vl-typecheck/src/lib.rs:783`](../crates/vl-typecheck/src/lib.rs#L783) — 🔴 **Bug:** Nested unknown types become `Array(Error)`, bypass `ty == Ty::Error`, and produce no diagnostic; `Array.new::[Array[Bogus]](1)` checks clean and is omitted from LIR. Detect nested poison recursively.

4. [`crates/vl-typecheck/src/lib.rs:1191`](../crates/vl-typecheck/src/lib.rs#L1191) — 🔴 **Bug:** Contextual integer coercion performs no range validation; `let x: u8 = 300;` checks clean and lowers to `44u8`. Validate the literal before recording the target type.

5. [`crates/vl-typecheck/src/lib.rs:1748`](../crates/vl-typecheck/src/lib.rs#L1748) — 🔴 **Bug:** Escaped `Ty::Int` values are compatible with every integer type without conversion; an `int` variable containing 300 can be passed to a `u8` parameter. Resolve `Int` at bindings or restrict this compatibility to literals actually coerced.

6. [`crates/vl-typecheck/src/lib.rs:917`](../crates/vl-typecheck/src/lib.rs#L917) — 🔴 **Bug:** Generic inference commits the first `Int` constraint, making argument order observable: `same(1u64, 2)` passes while `same(1, 2u64)` fails. Defer literal constraints and prefer a concrete integer constraint.

7. [`crates/vl-typecheck/src/lib.rs:1369`](../crates/vl-typecheck/src/lib.rs#L1369) — 🟡 **Risk:** External calls ignore explicit type arguments; `print::[u64]("hi")` checks clean although `print` is not generic. Reject turbofish arguments for non-generic externs.

8. [`crates/vl-typecheck/src/lib.rs:517`](../crates/vl-typecheck/src/lib.rs#L517) — 🟡 **Risk:** `return;` in a value function emits two E307 diagnostics because the invalid return does not suppress the later missing-return check. Mark it handled after reporting.

## Validation

All 57 existing `vl-typecheck` tests passed. The findings were reproduced separately through `vl check` and LIR emission.

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

## Fix audit — 2026-09-19 18:34:29 UTC-05:00

### Original finding status

1. **Verified fixed:** non-void functions now require every reachable path to return.
2. **Core hang fixed; follow-up remains:** type-expanding recursion now stops with E303, but the limit check can reject a duplicate instance at the valid boundary.
3. **Verified fixed:** nested `Array[Error]` poison is detected recursively and reported.
4. **Verified fixed:** contextual integer literals are range-checked; `let x: u8 = 300;` now reports E302.
5. **Still open:** `Ty::Int` can escape through an inferred generic call result and cross a concrete integer boundary without conversion.
6. **Verified fixed:** mixed `int`/concrete generic constraints infer independently of argument order.
7. **Verified fixed:** turbofish arguments on non-generic externs are rejected.
8. **Verified fixed:** bare `return;` in a value function now emits one E307 diagnostic.

### Remaining findings for handoff

1. [`crates/vl-typecheck/src/lib.rs:919`](../crates/vl-typecheck/src/lib.rs#L919) — 🔴 **Bug:** All-literal generic inference still returns `Ty::Int`; `function f(): u8 { return id(300); }` checks clean and lowers a `300int` result from `id$int` as `u8`. Default unresolved inferred `Int` arguments to `u64`, or stop treating non-literal `Int` expressions as compatible with every integer type.

2. [`crates/vl-typecheck/src/lib.rs:1088`](../crates/vl-typecheck/src/lib.rs#L1088) — 🔴 **Bug:** `MAX_INSTANCES` is checked before duplicate detection, so 64 distinct valid instances followed by a repeated call incorrectly emit the polymorphic-recursion error. Compute the mangled key and skip visited instances before enforcing the budget.

3. [`crates/vl-typecheck/src/lib.rs:395`](../crates/vl-typecheck/src/lib.rs#L395) — 🟡 **Risk:** The new path analysis reports that a function “has no `return` statement” even when one branch visibly returns. Report that not all reachable paths return and label the fallthrough construct.

4. [`crates/vl-typecheck/src/lib.rs:1934`](../crates/vl-typecheck/src/lib.rs#L1934) — 🟡 **Risk:** The 247-line fix adds no regression tests; the typechecker suite remains at 57 tests and does not catch the remaining generic-result bug or instance-limit boundary. Add focused unit tests for every original repro and both follow-up cases.

### Fix-audit validation

- `cargo test --workspace`: 219 passed.
- `cargo test -p vl-typecheck`: 57 passed.
- `cargo test -p vl-lir`: 12 passed.
- `cargo test --test pipeline`: 44 passed.
- All eight original repros were rerun through the rebuilt driver; seven now behave correctly, while finding 5 still accepts the invalid generic-result program.
- Type-expanding recursion terminates with E303 within the five-second probe timeout.

## Second fix audit — 2026-09-19 18:42:43 UTC-05:00

### Status

All eight original findings and all four findings from the first fix audit are verified fixed:

- `id(300)` now infers `id$u64`; returning it from `u8` reports E307, while returning it from `u64` emits `300u64` in LIR.
- A duplicate generic call at the 64-instance boundary checks clean; the 65th distinct instance still reports E303.
- Partial-return diagnostics now say that not all paths return and label the fallthrough construct.
- Twelve focused typechecker regression tests were added, bringing that suite from 57 to 69 tests.

### Remaining finding for handoff

1. [`crates/vl-typecheck/src/lib.rs:2572`](../crates/vl-typecheck/src/lib.rs#L2572) — 🟡 **Risk:** `expanding_recursion_hits_the_instance_budget` uses a value-returning `grow[T]` whose body already violates its declared return type, so the test emits both E307 and E303 and does not enforce the repository's single-root-error rule. Use the original void-function repro (`function grow[T](x: T) { grow([x]); }`) and assert exactly one E303 diagnostic.

### Second-audit validation

- `cargo test --workspace`: 231 passed.
- `cargo test -p vl-typecheck` (as part of the workspace run): 69 passed.
- Direct driver probes verified the generic-result rejection, `id$u64` LIR, corrected fallthrough diagnostic, and 64-instance duplicate boundary.
- The original void expanding-recursion repro emits exactly one E303 diagnostic.

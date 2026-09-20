# Type system next steps

The next priority is to stabilize inference boundaries before adding major new types.

## 1. Finish the audit cleanup

Replace the polymorphic-recursion regression with the original void-function repro, assert exactly one E303 diagnostic, and run `./scripts/check.sh`.

## 2. Enforce a normalized-type invariant

After typechecking, emitted code should contain no unresolved `Ty::Int`, `Ty::Param`, or nested `Ty::Error`. Add a validation helper and tests at the typecheck-to-LIR boundary so invalid types cannot silently disappear during lowering.

## 3. Centralize integer coercion

Move literal defaulting, range checks, and compatibility into one coercion operation. After coercion, ordinary compatibility should mostly use exact type equality instead of a general `Int` exception.

## 4. Make generic inference constraint-based

Collect all constraints before solving them, then default unresolved integer literals. This makes inference independent of argument order and prepares it for nested constraints and contextual result inference.

## 5. Split monomorphization into its own pass

Separate instance expansion from type checking. The dedicated pass should own instance caching, poisoned-template suppression, expanding-recursion diagnostics, and resource budgets.

## 6. Generalize control-flow analysis

Replace the return-only boolean analysis with a flow result such as:

```text
FallsThrough | Returns | Breaks | Continues
```

This provides a foundation for unreachable-code warnings and future control-flow constructs such as `match`.

## 7. Define numeric conversion semantics

Add explicit TypeScript-like casts such as `value as u8`. Specify whether overflow traps, wraps, or produces a diagnostic, and keep implicit cross-integer conversions narrow.

## 8. Add constrained generics

Opaque `T` values currently cannot support operators. Constraints such as `T extends Numeric` or `T extends Comparable` would enable useful generic algorithms without introducing full subtyping.

## Later language features

Once these foundations are stable, add user-defined structs and enums, particularly `Option`- and `Result`-style types. Defer full Hindley–Milner inference and structural subtyping until the simpler annotation-driven system requires them.

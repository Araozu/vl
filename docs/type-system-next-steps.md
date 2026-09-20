# Type system status and next steps

The original type-system roadmap is complete through constrained generics,
explicit integer casts, flow analysis, and separate monomorphization. The
current type checker also understands nominal, reference-semantic objects.

## Current object model

Objects are declared with:

```text
type Counter = object {
    value: u64,
    label: String,
};
```

An object literal initializes every field exactly once. Objects are heap-backed
reference values: assignment, parameters, and returns alias the same object,
and field writes are visible through every alias. The type is nominal, and the
current language intentionally provides only fields and reference semantics —
there are no implicit constructors, methods, inheritance, runtime casts, or
object identity/equality operators yet.

The implementation spans the full pipeline: lexer tokens, syntax and grammar
references, name resolution, HIR/type checking, LIR field operations, and the
Naravm container backend. Keep the grammar references in
`crates/vl-lex/GRAMMAR.md` and `crates/vl-syntax/GRAMMAR.md` in sync with any
future syntax changes.

## Next priorities

1. Add fallible values and diagnostics for externs such as `open` and `read`
   (`Option`/`Result` or an equivalent design).
2. Add enums and algebraic data, including the representation and control-flow
   rules needed for `Option`- and `Result`-style APIs.
3. Decide which object ergonomics belong in the language: constructors,
   methods, identity/equality, and runtime type information.
4. Continue expanding backend/runtime coverage while keeping object operations
   target-neutral in LIR.

Full Hindley–Milner inference and structural subtyping remain deferred until
the annotation-driven, nominal type system needs them.

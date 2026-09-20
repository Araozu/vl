# `var` and `val` bindings

This document is the implementation contract for VL's binding and reference
semantics. The feature is implemented across the lexer, parser, resolver, HIR,
type checker, LIR, examples, tests, editors, and website. It is kept here as
the internal reference for future language changes.

## Surface syntax

`let` is not a VL keyword. Bindings use `var` or `val`:

```vl
var counter = Counter { value = 0 }; // inferred *Counter
val snapshot = Counter { value = 0 }; // inferred Counter
var view: Counter = snapshot;          // rebindable, read-only reference
val handle: *Counter = counter;        // fixed, mutable reference view
```

Both declarations require a name, optional type annotation, initializer, and
semicolon:

```text
binding_item := ("var" | "val") ident (":" type)? "=" expr ";"
binding_stmt := ("var" | "val") ident (":" type)? "=" expr ";"
```

`var` means the binding can later be assigned a compatible value. `val` means
the binding itself cannot be assigned after initialization. This is independent
of the capability of a referenced value: a fixed `val x: *Foo` may mutate its
referent, while a rebindable `var x: Foo` may only read through it.

Function parameters remain fixed bindings. Their type still controls whether
the referent can be mutated:

```vl
fun inspect(value: Foo) { /* value = other; is an error */ }
fun edit(value: *Foo) { value.field = 1; /* value = other; is an error */ }
```

## Reference capability

VL reference types are `String`, `File`, named objects, and `Array[T]`.
Unstarred references (`Foo`) are read-only views. A leading `*` (`*Foo`) is a
mutable view of the same GC allocation. `*` is not a raw pointer, does not
change representation, and does not imply ownership or exclusive access.

The only implicit capability conversion is directional:

```text
*Foo -> Foo    allowed (discard mutation authority)
Foo  -> *Foo   rejected (cannot invent mutation authority)
```

The rule applies to initializers, assignments, function arguments and returns,
object fields, array elements, and generic type arguments. A read-only alias
still observes writes made through another mutable alias. Field and index writes
require a mutable receiver (`*Object` or `*Array[T]`); reads work through either
capability and project nested capabilities transitively.

Primitive types (`u64`, `i64`, `u8`, `f64`, and `bool`) cannot be prefixed with
`*`. Repeated qualifiers (`**Foo`), `*void`, and unconstrained `*T` are invalid.

## Inference rules

An explicit annotation is authoritative for capability. Without an annotation:

- an unannotated `var` receiving a fresh reference allocation infers a mutable
  view (`*Thing`);
- an unannotated `val` receiving a fresh reference allocation infers a
  read-only view (`Thing`);
- primitives infer their ordinary primitive type;
- an existing expression keeps the capability supplied by its type. In
  particular, an unannotated `var` cannot upgrade a read-only function result;
- fresh object literals, array literals, `Array.new`, and string literals may
  adopt a mutable expected type. `File` capability comes from the function
  signature that returns the handle and cannot be invented by a binding.

Examples:

```vl
var a = Foo {};             // *Foo
val b = Foo {};             // Foo
var c: Foo = Foo {};        // Foo, but rebindable
val d: *Foo = Foo {};       // *Foo, but fixed
var text = "hello";        // *String (fresh allocation)
var result = read_only();  // keeps the function's declared read-only type
```

After initialization, a binding's inferred or annotated type is fixed. A
`var` reassignment uses the same directional coercion rule; `val` reassignment
is rejected before type compatibility is considered.

## Compiler pipeline

- `vl-lex` emits `Var` and `Val`; `Let` is not lexed. `*` remains the same
  token in multiplication and type positions.
- `vl-syntax` stores `BindingKind::{Var, Val}` on top-level and statement
  binding nodes and parses both binding forms in every declaration position.
- `vl-semantic` records the binding kind on definitions, rejects direct writes
  to `val` bindings and parameters, and still resolves their right-hand sides.
  Field/index writes are left for capability checking.
- `vl-hir` preserves the binding kind and all qualified source types.
- `vl-typecheck` tracks fixed definitions, applies fresh-allocation inference,
  directional capability coercion, read-only projection, and mutation checks.
- `vl-lir` and code generation erase capability qualifiers because `Foo` and
  `*Foo` have the same runtime representation. Accepted rebinding remains an
  ordinary local copy; rejected writes never reach LIR.

Diagnostics remain `vl_common::Diagnostic` values and are emitted by the
driver. Important codes include E205 for fixed binding/parameter assignment,
E309 for incompatible binding flow, and E310 for mutation through a read-only
view. Error recovery poisons the affected node and suppresses cascades.

## Repository and tooling contract

The authoritative grammar documents are `crates/vl-lex/GRAMMAR.md` and
`crates/vl-syntax/GRAMMAR.md`. The same `var`/`val` vocabulary is maintained in:

- VS Code TextMate syntax (`editors/vscode/syntaxes/vl.tmLanguage.json`);
- Neovim syntax and Tree-sitter highlight queries;
- the website Shiki/TextMate grammar (`website/src/grammars/vl.tmLanguage.json`);
- website language pages and standard-library snippets;
- examples, compiler fixtures, and the `vlc` integration fixture.

When adding a binding example, choose `var` only when the name is rebound or a
fresh mutable view is required. Use `val` for fixed names and read-only views.
Use an explicit `*` annotation when the intended capability must be visible or
when the initializer is not a fresh allocation.

Validation commands:

```sh
./scripts/check.sh
(cd website && pnpm check && pnpm build)
(cd vlc && go test ./...)
```

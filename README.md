# VL — vibecoded language

Bootstrap compiler for a small expression language. Split-crate pipeline,
Ariadne error reporting, and a Naravm backend with target-neutral LIR.

## Architecture

```text
.vl source
  │  vl-lex        text -> tokens (hand-rolled, never panics)
  ▼  vl-syntax     tokens -> AST (recursive descent, per-item recovery)
  │  vl-semantic   AST -> name resolution (scopes, undefined/duplicate defs)
  ▼  vl-hir        resolved AST -> HIR (desugared, node ids, DefId links)
  │  vl-typecheck  HIR -> types (scalars, `String`s, `File`, `Array[T]`, objects)
  ▼  vl-lir        typed HIR -> three-address code (target-agnostic)
  │  vl-codegen    LIR -> backend output via `Target` trait
  ▼
  vl (driver)      CLI wiring + the ONLY place that prints diagnostics
  vl-common        spans, source table, Ariadne-backed `Diagnostic`
```

Dependency rule: each crate depends only on stages below it; everything
may depend on `vl-common`; nothing depends on the driver. `vl-lir` stays
target-agnostic — new targets mean new `Target` impls in `vl-codegen`.

The canonical lexical and syntax references are
[`vl-lex/GRAMMAR.md`](crates/vl-lex/GRAMMAR.md) and
[`vl-syntax/GRAMMAR.md`](crates/vl-syntax/GRAMMAR.md). Keep them aligned with
the token definitions and recursive-descent parser when the language changes.

## Requirements

- Rust stable (1.80+), Cargo. No other toolchain deps.

## Quick start

```sh
cargo run -- check examples/hello.vl        # frontend end-to-end
cargo run -- build examples/hello.vl        # compile (Naravm backend)
cargo run -- build examples/arith.vl --emit lir
cargo run -- build examples/arith.vl --target stackvm
cargo run -- lex examples/hello.vl
cargo run -- parse examples/hello.vl
cargo run -- targets                         # list backends
```

Error demo (pretty Ariadne output, exit 1):

```sh
cargo run -- check examples/err_undefined.vl
```

## Testing / checking path

| Command | What |
|---|---|
| `./scripts/check.sh` | **the gate**: fmt + check + clippy + test + driver smoke |
| `cargo test --workspace` | unit tests per crate + `tests/pipeline.rs` integration |
| `cargo check --workspace --all-targets` | fast typecheck |
| `cargo clippy --workspace --all-targets -- -D warnings` | lints, deny warnings |
| `cargo fmt --all -- --check` | formatting gate |

Golden tests: `tests/pipeline.rs` compiles `examples/*.vl` and diffs
`examples/arith.vl` against `tests/golden/arith.lir`. Regenerate a golden
after an *intended* LIR change with:

```sh
cargo run -q -- build examples/arith.vl --emit lir > tests/golden/arith.lir
```

then eyeball the diff before committing.

## Error reporting (hard requirement)

All user-facing errors are `vl_common::Diagnostic` rendered with
[Ariadne](https://crates.io/crates/ariadne). Stages return
`Vec<Diagnostic>` and keep going; only `src/main.rs` prints (stderr,
colours) and sets the exit code. Never add another reporting library.

## Language v0 (`examples/`)

```text
let x = 1 + 2 * 3;
fun main() { let d = x - 1; }
```

Integer literals are untyped and coerce to contextual `u64`, `i64`, or `u8`;
floating literals retain the `f64` suffix. Boolean literals are `true` and `false`.
Primitive value types are `u64`, `i64`, `f64`, `bool`, and `u8`. `String`,
`File`, user-defined `object` types, and `Array[T]` are reference types; `void`
is only valid as a return type. `Array[T]` is a read-only view of a fixed-length
heap array of `T`, while `*Array[T]` is the mutable view: `Array.new::[u64](n)`
allocates a zero-filled array of `n` elements (no import needed), `[1, 2]` is an
array literal, `a[i]` reads an element, and `a[i] = v;` writes through a
`*Array[T]` view (see `examples/arrays.vl`). Indices are always `u64`.
Objects use declarations such as `type Counter = object { value: u64, label: String, };`
and named literals such as `Counter { value = 1, label = "count" }`. Object values
have reference semantics: assignment, parameters, and returns alias the same
heap object, and `p.field = value;` mutates it through every mutable `*Foo` alias. Fields are
comma-separated and every field must be initialized. Objects are nominal data
types; VL does not currently attach methods, inheritance, or runtime type
reflection to them.
Functions can declare type parameters (`fun first[T](a: Array[T]): T`);
calls infer them (`first(a)`) or pass them explicitly (`first::[u64](a)`)
(see `examples/generics.vl`).
The language also supports double-quoted byte strings, `+ - * /`, unary `-`
and `!`, comparisons (`== != < <= > >=`), short-circuit `&&` / `||`, parens,
`let` plus `=` reassignment, user-defined functions with typed boundaries
(`fun add(a: i64, b: i64): i64 { return a + b; }`; an omitted return
type means `void`), explicit `return` (`return <expr>;` for values,
`return;` for `void`; there are no implicit returns — a trailing expression
is discarded, never returned),
calls between user functions (nestable, order-independent, recursive;
arity and argument types are checked, `void` results only as bare statements),
`if`/`else` conditionals with mandatory parentheses, and `while` loops
with `break` / `continue`. A runnable program defines a zero-argument
`fun main()` returning `void` as its entrypoint; `main` is optional at
compile time (snippets and libraries compile without one — entrypoint
presence is validated by the VM/loader, not the compiler). Other functions compile
to their own Nara functions on the Naravm target (see `examples/calls.vl`).
Branches may be single statements or brace-delimited blocks. `//` comments.
String escapes are `\\0`,
`\\n`, `\\r`, `\\t`, `\\\\`, and `\\"`; strings may not cross a newline.
Semicolons are mandatory. Strings are carried as bytes through LIR. Extern
signatures (`std.print`, `std.fs`, `std.string`) are declared in
`vl-codegen::modules` and enforced by `vl-typecheck`; backends map VL types to
target concepts. See the [lexical grammar](crates/vl-lex/GRAMMAR.md) and
[syntax grammar](crates/vl-syntax/GRAMMAR.md) for the complete grammar.

## Roadmap

1. Harden/add more `vl-codegen` backends.
2. Fallible externs (`!File`, `!String` via `errno`/`0x30`) once VL gains error handling.
3. Bytecode/assembly emission + runner.

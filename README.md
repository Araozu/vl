# VL — vibecoded language

Bootstrap compiler for a small expression language. Split-crate pipeline,
Ariadne error reporting, target platform TBD.

## Architecture

```text
.vl source
  │  vl-lex        text -> tokens (hand-rolled, never panics)
  ▼  vl-syntax     tokens -> AST (recursive descent, per-item recovery)
  │  vl-semantic   AST -> name resolution (scopes, undefined/duplicate defs)
  ▼  vl-hir        resolved AST -> HIR (desugared, node ids, DefId links)
  │  vl-typecheck  HIR -> types (v0: everything is `int`)
  ▼  vl-lir        typed HIR -> three-address code (target-agnostic)
  │  vl-codegen    LIR -> backend output via `Target` trait
  ▼
  vl (driver)      CLI wiring + the ONLY place that prints diagnostics
  vl-common        spans, source table, Ariadne-backed `Diagnostic`
```

Dependency rule: each crate depends only on stages below it; everything
may depend on `vl-common`; nothing depends on the driver. `vl-lir` stays
target-agnostic — new targets mean new `Target` impls in `vl-codegen`.

## Requirements

- Rust stable (1.80+), Cargo. No other toolchain deps.

## Quick start

```sh
cargo run -- check examples/hello.vl        # frontend end-to-end
cargo run -- build examples/arith.vl        # compile (dummy backend)
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
function main() { let d = x - 1; d; }
```

Ints, `+ - * /`, unary `-`, parens, `let`, `function` with params and calls,
`//` comments.
Semicolons are mandatory. Type system: everything is `int`. See crate docs for the grammar.

## Roadmap

1. Decide target platform → harden/add a `vl-codegen` backend.
2. Thread locals through LIR (vars currently materialise as consts).
3. Grow types (`bool`, `string`, function types) in `vl-typecheck`.
4. Bytecode/assembly emission + runner.

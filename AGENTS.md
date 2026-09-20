# AGENTS.md — working in this repo

Read this before touching code. Short on purpose.

## Language

VL surface syntax uses `let`, `fun`, braces, and `//` comments.
Semicolons are mandatory.

## Layout

Workspace crates (dependency order, lower first):

`vl-common` → `vl-lex` → `vl-syntax` → `vl-semantic` → `vl-hir` →
`vl-typecheck` → `vl-lir` → `vl-codegen`, plus the `vl` driver binary
(`src/main.rs`) that wires them together.

- `vl-common`: `Span`, `Sources`, `Diagnostic`. Everyone depends on it.
- Driver (`src/main.rs`): owns CLI, file I/O, exit codes, printing.
- `examples/*.vl`: sample programs. `err_*.vl` must FAIL.
- `tests/pipeline.rs` + `tests/golden/`: integration + golden tests.

## Naravm target

The target VM checkout is at `~/projects/zig/naravm`. Inspect its ISA and file format when
implementing the Naravm backend, but **never modify files in that checkout**.
All VL-side integration belongs in this repository, including
`crates/vl-codegen`, `vlc/`, and website deployment configuration.

## Hard rules

1. **Errors via Ariadne only.** Produce `vl_common::Diagnostic`, print with
   `emit_all` in the driver. Do not add miette / custom renderers.
2. **Respect the pipeline.** No crate imports from a later stage; no
   target-specific code outside `vl-codegen`; no printing inside libraries
   (return diagnostics, let the driver emit).
3. **Recovery, not panic.** Lex/parse/resolve collect diagnostics per item
   and continue. `.unwrap()` on user input is a bug; on truly invariant
   violations use `expect("why this is impossible")`.
4. **Poison, don't cascade.** After an error, mark nodes `Ty::Error` /
   `def: None` and stay quiet downstream — one root cause, one error.

## Workflow

```sh
./scripts/check.sh            # the gate (fmt, check, clippy, test, smoke)
cargo test -p vl-<crate>      # fast loop on one crate
cargo run -- check examples/hello.vl
cargo run -- build examples/arith.vl --emit lir
```

For website commands, use the system-installed `pnpm` directly. Do not invoke
Corepack or use `corepack pnpm`.

Golden update (only for intended LIR changes, review the diff):

```sh
cargo run -q -- build examples/arith.vl --emit lir > tests/golden/arith.lir
```

## Adding a language feature

1. `vl-lex`: tokens. 2. `vl-syntax`: AST + grammar doc. 3. `vl-semantic`:
   scoping. 4. `vl-hir`: desugar + lower. 5. `vl-typecheck`: `Ty` + rules.
   6. `vl-lir`: instructions. 7. `vl-codegen`: backend support.
   Cover each touched stage with a unit test; extend `tests/pipeline.rs`
   and `examples/` for end-to-end behaviour.

## Adding a backend

New file/type in `crates/vl-codegen` implementing `Target`, register in
`lookup()` + `all_targets()`. Never branch the driver or LIR on target
names. Naravm is currently the sole target.

## Commit style

Conventional commits (`feat(vl-syntax): …`, `fix(vl-lex): …`,
`docs: …`). One pipeline stage per commit where possible.

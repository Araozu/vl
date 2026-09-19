---
layout: ../../layouts/Docs.astro
title: CLI reference
description: Check, build, and inspect VL programs from the command line.
eyebrow: Reference
availability: VL 0.1+
---

# CLI reference

The `vl` driver reads a `.vl` file, runs the compiler stages, and reports
diagnostics. Run it from the repository root with `cargo run --`; once a binary
is installed, replace that prefix with `vl`.

## Commands

### `check <file>`

Validate a source file without producing a target artifact.

```sh
cargo run -- check examples/hello.vl
```

The command exits 0 when the file is valid. Errors are printed with source
locations and the command exits 1.

### `build <file>`

Compile a source file. The default target is `naravm`.

```sh
cargo run -- build examples/hello.vl --out hello.nara
```

Use `--out <path>` to write the result to a file. Without it, text output is
written to stdout; binary target output is also written to stdout, so a file is
usually the safer choice for Naravm.

### `lex <file>`

Print the tokens produced from a source file. This is useful when investigating
comments, literals, or punctuation.

```sh
cargo run -- lex examples/hello.vl
```

### `parse <file>`

Print the parsed syntax tree without running the complete compiler pipeline.

```sh
cargo run -- parse examples/hello.vl
```

### `targets`

List the code-generation targets known to the compiler:

```sh
cargo run -- targets
```

`naravm` produces a runnable Naravm vmfile. `dummy` and `stackvm` are text
inspection backends; `stackvm` is a sketch and is not executable.

## Build options

`build` accepts these options:

| Option | Purpose |
| --- | --- |
| `--target <name>` | Select `naravm`, `dummy`, or `stackvm`. |
| `--emit <kind>` | Dump `tokens`, `ast`, `lir`, or `asm` instead of the final artifact. |
| `--out <path>` | Write output to a file instead of stdout. |

Examples:

```sh
cargo run -- build examples/arith.vl --emit lir
cargo run -- build examples/arith.vl --target dummy --emit asm
cargo run -- build examples/hello.vl --target naravm --out /tmp/hello.nara
```

For the language itself, continue with the [learning path](/docs). For module
and function details, see the [standard library](/std).

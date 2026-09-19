---
layout: ../../layouts/Docs.astro
title: CLI reference
description: Every subcommand of the vl driver, with examples.
eyebrow: Reference
availability: VL 0.1+
---

# CLI reference

The subcommands below are real; the driver (`src/main.rs`) owns the flags, the
file reading, the exit codes, and all printing. `main()` is required for
`check` and `build`.

```text
vl check <file> [--emit lir] [--target <name>]
```

## Topics

### `check <file>`

Runs the frontend end to end. Prints nothing on success; on failure, an Ariadne report on stderr and exit 1.

```sh
cargo run -- check examples/hello.vl
```

### `build <file> [--emit lir] [--target <name>]`

Compiles through the selected backend. The default backend is `naravm`, which
writes an executable Naravm 0.2 vmfile. Use `--out` for binary output.

```sh
cargo run -- build examples/hello.vl --out hello.nara
cargo run -- build examples/arith.vl --emit lir
```

The available targets are `naravm`, `dummy`, and `stackvm`. The latter two are
inspection backends; only `naravm` produces a runnable vmfile.

### `lex <file>`

Inspection helper that stops after tokens.

### `parse <file>`

Inspection helper that stops after the tree.

### `targets`

Lists the backends registered in `vl-codegen`.

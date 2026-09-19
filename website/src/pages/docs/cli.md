---
layout: ../../layouts/Docs.astro
title: CLI reference
description: Every subcommand of the vl driver, with examples.
eyebrow: Reference
availability: VL 0.1+
---

# CLI reference

The subcommands below are real; the driver (`src/main.rs`) owns the flags, the file reading, the exit codes, and all printing.

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

Compiles through the selected backend. The default backend is a placeholder until a real target lands.

```sh
cargo run -- build examples/arith.vl --emit lir
```

### `lex <file>`

Inspection helper that stops after tokens.

### `parse <file>`

Inspection helper that stops after the tree.

### `targets`

Lists the backends registered in `vl-codegen`.

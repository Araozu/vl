---
layout: ../../layouts/Docs.astro
title: Driver
description: How the VL command-line driver wires the compiler together.
eyebrow: Internals
availability: VL 0.1+
---

# Driver

The driver is `src/main.rs` at the workspace root. It owns the command line,
file I/O, exit codes, and all diagnostic printing.

## Responsibilities

- Parse `init`, `check`, `build`, `run`, `lex`, `parse`, `fmt`, `lsp`, and
  `targets`.
- Read source files or stdin and pass their names and text into the pipeline.
- Select a target for `build`, render diagnostics as human text or JSON, and
  write text or binary output.
- Format `.vl` files through `vl-fmt` and run project scripts through the
  configured shell.
- Start `vl-lsp` over stdio for editor integrations.
- Render human diagnostics with Ariadne; serialize JSON or LSP diagnostics for
  tools when requested.

Libraries return diagnostics; they do not print directly. The [CLI reference](/cli)
documents the user-facing commands.

`check` and `build` accept stdin (`-`) and a JSON diagnostics format for tools.
The language server works on editor buffers through the in-memory
`vl-frontend` crate; it does not route buffer contents through the CLI file
reader.

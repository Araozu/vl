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

- Parse `check`, `build`, `lex`, `parse`, and `targets`.
- Read source files and pass their names and text into the pipeline.
- Select a target for `build` and write text or binary output.
- Render every `Diagnostic` with Ariadne.

Libraries return diagnostics; they do not print directly. The [CLI reference](/cli)
documents the user-facing commands.

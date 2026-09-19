---
layout: ../../layouts/Docs.astro
title: Compiler internals
description: The VL compiler pipeline, diagnostics, and target boundary.
eyebrow: For contributors
availability: VL 0.1+
---

# Compiler internals

This section is for contributors and tools authors who need to understand how a
VL program moves through the compiler. If you are learning VL, start with the
[learning path](/docs). For modules and functions used by programs, see the
[standard library](/std).

## The pipeline

Each stage has one job and depends only on earlier stages:

```text
.vl
  → vl-lex        text to tokens
  → vl-syntax     tokens to a syntax tree
  → vl-semantic   names to definitions and scopes
  → vl-hir        resolved tree to a desugared HIR
  → vl-typecheck  HIR to typed HIR
  → vl-lir        typed HIR to target-neutral three-address code
  → vl-codegen    LIR to target output
```

The `vl-common` crate provides spans, source tables, and diagnostics to every
stage. The driver in `src/main.rs` wires the stages together.

## Diagnostics and recovery

Stages return `Vec<Diagnostic>` and continue recovering where they can. The
driver is the only layer that renders diagnostics with Ariadne and chooses the
process exit code. Libraries do not print directly.

When a stage cannot produce a meaningful node, later stages receive a poisoned
value such as `Ty::Error` or a missing definition. That keeps one root cause
from becoming a cascade of repeated errors.

## Target boundary

`vl-lir` stays independent of any target platform. Backends implement the
`Target` trait in `vl-codegen` and are registered by name. The current targets
are `naravm`, `dummy`, and `stackvm`; only `naravm` produces a runnable vmfile.

The driver selects a target at build time with `--target`. The frontend and
LIR do not branch on target names, which keeps adding a backend local to
`vl-codegen`.

## Where to look next

- [Driver](/internals/driver): CLI wiring, file I/O, diagnostics, and exit codes.
- [Frontend crates](/internals/frontend): shared types, lexing, parsing, and name resolution.
- [Backend crates](/internals/backend): HIR, type checking, LIR, and code generation.
- [Compiler service](/internals/compiler-service): the HTTP endpoint used by the playground.

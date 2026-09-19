---
layout: ../../layouts/Docs.astro
title: Backend crates
description: The VL stages from a resolved tree to target output.
eyebrow: Internals
availability: VL 0.1+
---

# Backend crates

The backend stages turn a resolved program into target-neutral code and then
into target output:

```text
vl-hir → vl-typecheck → vl-lir → vl-codegen
```

## `vl-hir`

Lowers the resolved syntax tree into a desugared tree with node ids and
`DefId` links.

## `vl-typecheck`

Checks `u64`, `i64`, `f64`, `bool`, `u8`, and byte-string values, producing
typed HIR and diagnostics.

## `vl-lir`

Lowers typed HIR to target-agnostic three-address code. It does not know which
backend will consume the program.

## `vl-codegen`

Defines the `Target` trait and registers `naravm`, `dummy`, and `stackvm`.
`naravm` serializes Naravm 0.2 vmfiles; the other targets are inspection
backends.

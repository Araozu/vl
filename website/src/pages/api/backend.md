---
layout: ../../layouts/Docs.astro
title: Backend crates
description: A resolved tree in, backend output out.
eyebrow: Reference
availability: VL 0.1+
section: api
---

# Backend crates

A resolved tree in, backend output out.

```text
vl-hir → vl-typecheck → vl-lir → vl-codegen
```

## Topics

### `vl-hir`

The desugared tree, with node ids and `DefId` links. Stub: the node catalogue.

### `vl-typecheck`

Types and their rules. v0 has `int` and byte-string values. String codegen is
not implemented yet; strings currently stop at target-neutral LIR. Stub: the
full judgments.

### `vl-lir`

Target-agnostic three-address code. Stub: the instruction set.

### `vl-codegen`

Backends implement `Target` and register in `lookup` and `all_targets`. The placeholder backend stays until a real one takes its place as default. Stub: how to add a backend.

Backends also publish the target module catalog. The current catalog includes
`std.fs` (`open`, `read`) and `std.string` (`new`, `len`). Frontend imports are
checked against this catalog, while LIR retains the source module name for
backend placement.

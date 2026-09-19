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

Types and their rules. VL has `u64`, `i64`, `f64`, `bool`, `u8`, and byte-string values. String literals
reach target-neutral LIR and the Naravm backend can emit them for `std.print`.
Stub: the full judgments.

### `vl-lir`

Target-agnostic three-address code. Stub: the instruction set.

### `vl-codegen`

Backends implement `Target` and register in `lookup` and `all_targets`.
`NaraVmTarget` serializes Naravm 0.2 vmfiles and emits source `main` as the
special `<entrypoint>` function required by the VM. `DummyTarget` and
`StackVmTarget` remain inspection backends.

Backends also publish the target module catalog. The current catalog includes
`std` (`print`, `print_u64`), `std.fs` (`open`, `read`), and `std.string`
(`new`, `len`). Frontend imports are checked against this catalog, while LIR
retains dotted source names for backend placement. Naravm uses double-colon
names internally; the backend performs that target-specific mapping.

---
layout: ../../layouts/Docs.astro
title: Frontend crates
description: Text in, resolved names out.
eyebrow: Reference
availability: VL 0.1+
section: api
---

# Frontend crates

Text in, resolved names out.

```text
.vl → vl-lex → vl-syntax → vl-semantic
```

## Topics

### `vl-common`

`Span`, `Sources`, and the Ariadne-backed `Diagnostic`. Every crate builds on it.

### `vl-lex`

A hand-rolled tokenizer that never panics. Stub: the token table.

### `vl-syntax`

A recursive-descent parser and its tree, with per-item recovery. Stub: the node catalogue and grammar notes.

### `vl-semantic`

Scope resolution: undefined names and duplicate definitions. Stub: the resolution rules.

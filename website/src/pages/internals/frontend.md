---
layout: ../../layouts/Docs.astro
title: Frontend crates
description: The VL stages from source text to resolved names.
eyebrow: Internals
availability: VL 0.1+
---

# Frontend crates

The frontend turns source text into a tree with resolved names:

```text
.vl → vl-lex → vl-syntax → vl-semantic
```

## `vl-common`

Provides `Span`, `Sources`, and the Ariadne-backed `Diagnostic` type used by
the rest of the workspace.

## `vl-lex`

Turns source text into tokens and reports lexical errors while continuing where
possible.

## `vl-syntax`

Parses tokens into the syntax tree and recovers per item so one malformed item
does not prevent the rest of a file from being inspected.

## `vl-semantic`

Resolves names and imports, reporting undefined names and duplicate definitions
before later stages consume the tree.

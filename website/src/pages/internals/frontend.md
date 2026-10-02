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
does not prevent the rest of a file from being inspected. The grammar includes
object, union, and error-set declarations; associated functions inside object
bodies; arrays and tuples; nullable and fallible types; destructuring; and
`match` patterns.

## `vl-semantic`

Resolves names and imports, reporting undefined names and duplicate definitions
before later stages consume the tree. It resolves object type names and
`Type.method` callees while leaving object layout and field-type checks to
`vl-typecheck`. A `receiver.method(args)` call whose head is a bound value is
recorded as instance sugar; the self-type gate is validated later.

## `vl-frontend`

Provides in-memory `check_text` over lexing through type checking and world
planning. Callers supply a pre-merged module catalog and optional compiled
world references; the crate does no filesystem I/O. `LineIndex` converts byte
spans into line and column locations, and JSON diagnostic types expose codes,
labels, notes, and byte plus line/column ranges for editor protocols.

`vl` uses this crate for machine-readable diagnostics, and `vl-lsp` uses it to
analyze editor buffers. See [Language server](/internals/language-server).

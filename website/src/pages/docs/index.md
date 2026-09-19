---
layout: ../../layouts/Docs.astro
title: Documentation
description: Start here. Three short chapters cover VL v0 end to end.
eyebrow: Collection
availability: VL 0.1+
---

# Documentation

Start here. In v0, VL is integers, arithmetic, `let`, `function`, and `//` comments, carried through a strict staged pipeline. Three short chapters cover it.

```text
.vl → vl-lex → vl-syntax → vl-semantic → vl-hir → vl-typecheck → vl-lir → vl-codegen
```

## Topics

### Getting started

Install nothing but Rust, then check and build your first `.vl` file.

[Getting started](/docs/getting-started)

### Language tour

The v0 surface, scoping, and why one root cause stays one error.

[Language tour](/docs/language)

### CLI reference

Every subcommand of the `vl` driver, with examples.

[CLI reference](/docs/cli)

## Overview

Each stage gets its own chapter once the language surface settles. For the crate view, see the [API reference](/api).

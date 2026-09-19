---
layout: ../../layouts/Docs.astro
title: Language tour
description: The v0 surface, scoping, and error promises.
eyebrow: Article
availability: VL 0.1+
---

# Language tour

A sketch of the v0 surface. For now, every value is an `int`.

```vl
let x = 1 + 2 * 3;
function main() { let d = x - 1; d; }
```

## Overview

Integers with `+ - * /`, unary minus, and parentheses. `let` binds a name, `function` takes parameters, `//` starts a comment that runs to the line end. Every statement ends with `;`.

Scopes reject two things: names nobody defined, and names defined twice. After an error the compiler marks its nodes and stays quiet downstream, so you fix causes, not echoes.

## Topics

### The grammar, with precedence

Stub. Statements, expressions, and where `;` is required.

### Scoping rules, stated precisely

Stub. Undefined names, duplicate definitions, and shadowing.

### Recovery and poisoning, with examples

Stub. Per-item recovery and why `Ty::Error` passes through quietly.

### What comes next

Stub. `bool`, `string`, and function types after v0.

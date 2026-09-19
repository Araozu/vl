---
layout: ../../layouts/Docs.astro
title: Language tour
description: The v0 surface, scoping, and error promises.
eyebrow: Article
availability: VL 0.1+
---

# Language tour

A sketch of the v0 surface. Values can be integers or byte strings.

```vl
let x = 1 + 2 * 3;
function main() { let d = x - 1; d; }
```

## Overview

Integers with `+ - * /`, unary minus, and parentheses, plus double-quoted
strings. Strings are raw bytes for now, not UTF-8 text. `let` binds a name,
`function` takes parameters, and calls use TypeScript-style `name(args)` syntax.
`//` starts a comment that runs to the line end. Every statement ends with `;`.

```vl
function add(a, b) { a + b; }
function main() { add(1, 2); }
```

### Strings

Strings use double quotes only. They may contain any byte except an unescaped
quote or a newline. The supported escapes are `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`,
and `\\"`.

```vl
let greeting = "hello\\nworld";
let quote = "say \\\"hi\\\"";
```

An unterminated string is a lexical error at the line where it starts. String
values reach LIR as byte arrays; code generation does not support them yet.

Scopes reject two things: names nobody defined, and names defined twice. After an error the compiler marks its nodes and stays quiet downstream, so you fix causes, not echoes.

## Topics

### The grammar, with precedence

Expressions include `int` and `string` literals. Arithmetic keeps its usual
precedence, and statements and bindings require `;`.

### Scoping rules, stated precisely

Stub. Undefined names, duplicate definitions, and shadowing.

### Recovery and poisoning, with examples

Stub. Per-item recovery and why `Ty::Error` passes through quietly.

### What comes next

Bool values, richer function types, and a backend representation for strings.

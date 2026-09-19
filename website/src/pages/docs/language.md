---
layout: ../../layouts/Docs.astro
title: Language tour
description: The v0 surface, scoping, and error promises.
eyebrow: Article
availability: VL 0.1+
---

# Language tour

A description of the VL surface. Values include typed scalars and byte strings.

```vl
use std;

function main() {
    std.print("Hello, world!\n");
}
```

## Overview

Numeric literals are `i64` by default or can use `u64`, `i64`, `f64`, and `u8`
suffixes. Boolean literals are `true` and `false`. Numeric values support
`+ - * /`, unary minus, and parentheses, plus double-quoted strings. Strings
are raw bytes for now, not UTF-8 text. `let` binds a name,
`function` takes parameters, and calls use TypeScript-style `name(args)` syntax.
Every program must define a zero-argument `function main()`; it becomes the
runtime entrypoint.
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
values reach LIR as byte arrays. The Naravm backend can currently lower string
literals passed to `std.print`.

Scopes reject two things: names nobody defined, and names defined twice. After an error the compiler marks its nodes and stays quiet downstream, so you fix causes, not echoes.

### Conditionals

Conditionals require parenthesized boolean conditions. Branch braces are
optional: each branch can be one statement or a brace-delimited block. `else`
is optional.

```vl
function main() {
    let ready = true;
    if (ready) {
        std.print("ready\n");
    } else {
        std.print("not ready\n");
    }
}
```

For a single statement, omit the braces:

```vl
if (ready) std.print("ready\n"); else std.print("not ready\n");
```

## Topics

### Modules and namespaces

Every `.vl` file is its own module. Its module name is the filename without the
extension, so `reader.vl` is module `reader`. Target backends publish modules
such as `std` and `std.fs`.

Import a module with a dotted Rust-style `use`:

```vl
use std.string;

function main() { string.new(); }
```

The import introduces only the final module name, not its descendants or
exports. Use grouped imports when individual exports should be direct names:

```vl
use std.fs.{open, read};

function main() { open(); read(); }
```

Unknown modules and exports are reported during name resolution. Qualified
calls reach LIR and backend output with their full dotted path (`string.new`
after the module import). Dots are VL syntax; Naravm receives the corresponding
`std::...` name at code generation time.

### Standard output

The Naravm standard module currently exposes `print` and `print_u64`:

```vl
use std;

function main() {
    std.print("hello\n");
}
```

`std.print` takes one string and does not add an implicit newline.

### The grammar, with precedence

Expressions include typed scalar and `string` literals. Arithmetic keeps its usual
precedence, and statements and bindings require `;`.

### Scoping rules, stated precisely

Stub. Undefined names, duplicate definitions, and shadowing.

### Recovery and poisoning, with examples

Stub. Per-item recovery and why `Ty::Error` passes through quietly.

### What comes next

Richer function types and a backend representation for all scalar values.

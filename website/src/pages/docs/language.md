---
layout: ../../layouts/Docs.astro
title: Language guide
description: The values, expressions, functions, and modules in VL 0.1.
eyebrow: Article
availability: VL 0.1+
---

# Language guide

VL uses a small, TypeScript-like surface: declarations use `let` and
`function`, blocks use braces, and statements end with semicolons.

```vl
use std.print;

function main() {
    print("Hello, world!\n");
}
```

## Values and expressions

Numeric literals are `i64` by default. Add a suffix when you need `u64`, `f64`,
or `u8`; boolean literals are `true` and `false`. Numeric values support `+`,
`-`, `*`, `/`, unary minus, and parentheses. Calls use the familiar `name(args)`
form.

```vl
let count = 255u8;
let total = 1u64;
let ratio = 1.5f64;
let result = (total + 2u64) * 3u64;
```

## Bindings and functions

Use `let` to bind a value and `function` to name reusable work. Parameters are
typed, and the return type follows the parameter list:

```vl
let greeting = "hello";

function add(a: i64, b: i64): i64 {
    a + b;
}

function main() {
    add(1, 2);
}
```

A missing return type means `void`, so `function main()` is
`function main(): void`. Calls check arity and argument types, and the declared
return must match the body's tail value; `void` results may only appear as bare
statements.

Names must be defined before they are used, and a scope cannot define the same
name twice. The compiler reports those problems at the source location.

Every program must define a zero-argument `function main()` returning `void`;
it becomes the runtime entrypoint.

## Strings

Strings use double quotes only. They may contain any byte except an unescaped
quote or a newline. Supported escapes are `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`,
and `\\"`.

```vl
let greeting = "hello\\nworld";
let quote = "say \\"hi\\"";
```

Strings are byte strings for now rather than a full text type. See the
[standard library](/std) for the functions that work with them.

## Conditionals

Conditionals require parenthesized boolean conditions. Branch braces
are optional: each branch can be one statement or a brace-delimited block, and
`else` is optional.

```vl
use std.print;

function main() {
    let ready = true;
    if (ready) {
        print("ready\n");
    } else {
        print("not ready\n");
    }
}
```

For a single statement, omit the braces:

```vl
if (ready) print("ready\n"); else print("not ready\n");
```

## Modules and imports

Each `.vl` file can import a module with a dotted `use` path:

```vl
use std.print;

function main() {
    print("hello\n");
}
```

A trailing export can be imported directly (`use std.print;` behaves like
`use std.{print};` and brings `print` into scope), and grouped imports can
bring selected exports into the current file:

```vl
use std.string.{len};

function main() {
    let n = len("hello");
    n;
}
```

Unknown modules and exports are reported when the file is checked. The
available modules and functions are listed in the [standard library](/std).

## Current limits

VL 0.1 is intentionally small: strings are byte strings, the standard modules
are limited, and Naravm is the only runnable target. Use the [CLI reference](/cli)
for inspection commands or [Compiler internals](/internals) for implementation
details.

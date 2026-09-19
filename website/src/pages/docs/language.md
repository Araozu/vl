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
`-`, `*`, `/`, unary minus, and parentheses. Comparisons use `==`, `!=`, `<`,
`<=`, `>`, `>=`; boolean values combine with `&&`, `||`, and prefix `!`.
`&&` and `||` short-circuit left to right. Calls use the familiar `name(args)`
form.

```vl
let ready = true;
let big = total >= 10u64;
let ok = ready && big || !ready;
```

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
    return a + b;
}

function main() {
    add(1, 2);
}
```

A missing return type means `void`, so `function main()` is
`function main(): void`. There are no implicit returns: only an explicit
`return <expr>;` yields a value (`return;` with no value is for `void`
functions). A trailing expression statement is discarded, never returned —
`function add(a: i64, b: i64): i64 { a + b; }` is an error (`E307`), not a
shorthand. The declared return must match every `return <expr>;` value;
`void` results may only appear as bare statements.

User functions are first-class callees: any `function` item can be called from
any other function body (or from itself, recursively), regardless of definition
order. Calls nest freely, and argument values are evaluated left to right:

```vl
function add(a: i64, b: i64): i64 {
    return a + b;
}

function twice(x: i64): i64 {
    return add(x, x);
}

function main() {
    twice(add(1, 2));
}
```

A call with the wrong number of arguments, or an argument of the wrong type,
is a single error pointing at the call. Calling something that is not a
`function` (for example a `let` binding), or binding a `void` result with
`let`, is an error too. See `examples/calls.vl` for a complete program.

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

## Assignment

Bindings created with `let` can be reassigned with `=`. The new value must
have the same type as the binding:

```vl
function main() {
    let count = 0;
    count = count + 1;
}
```

Assigning to an undefined name, or with a mismatched type, is an error.

## Loops

`while` repeats a branch while its parenthesized boolean condition holds.
Branch braces are optional, as with `if`. `break` exits the innermost loop
and `continue` jumps to its next iteration check:

```vl
use std.print_u64;

function main() {
    let i = 3u64;
    while (i > 0u64) {
        print_u64(i);
        i = i - 1u64;
    }
}
```

```vl
while (true) {
    if (done) {
        break;
    }
    tick();
    if (skip) {
        continue;
    }
}
```

`break` and `continue` outside a loop are errors.

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
}
```

Unknown modules and exports are reported when the file is checked. The
available modules and functions are listed in the [standard library](/std).

## Current limits

VL 0.1 is intentionally small: strings are byte strings, the standard modules
are limited, and Naravm is the only runnable target. The Naravm backend runs
every `function` item — `function main()` becomes the entrypoint and each
other function becomes its own Nara function — with integer arithmetic,
comparisons, and control flow plus `std.print` / `std.print_u64` and calls
between user functions (including recursion). Value parameters arrive in
`rv11` upwards and reference (`string`, `File`) parameters in `rf31` upwards;
at most 15 value and 9 reference parameters per function are supported. Float
ordering, string equality, and string ordering are rejected with a diagnostic.
Use the [CLI reference](/cli) for inspection commands or
[Compiler internals](/internals) for implementation details.

---
layout: ../../layouts/Docs.astro
title: Strings
description: Write text values, handle escapes, and use the standard string functions.
eyebrow: Learn the language
availability: VL 0.1+
---

# Strings

Strings hold text like `"hello"`. Use them for messages, names, and anything
you want to print.

```vl
use std;

fun main() {
    val name = "Ada";
    std.println(name);
}
```

`std.println` prints the string and ends the line. `std.print` prints exactly
what you give it, with no extra newline.

## Writing strings

Wrap the text in double quotes. A string cannot span multiple lines, and an
unescaped `"` always ends it.

```vl
val greeting = "hello world";
```

Some characters need an escape code:

```vl
val lines = "hello\nworld";
val quote = "say \"hi\"";
```

| Escape | Meaning |
| --- | --- |
| `\n` | New line |
| `\t` | Tab |
| `\r` | Carriage return |
| `\0` | Zero byte |
| `\\` | Backslash |
| `\"` | Double quote |

Anything else after a `\` is an error. Strings hold bytes, so `len` below
counts bytes rather than display characters.

## Working with strings

String helpers live in `std.string`. Import the module, then call them with
the module name in front:

```vl
use std.string;

fun main() {
    val message = "hello";
    val size = string.len(message);
    val empty = string.is_empty(message);
}
```

The most useful helpers are:

| Function | What it does |
| --- | --- |
| `string.len(s)` | Byte length of `s` |
| `string.is_empty(s)` | `true` when `s` has zero bytes |
| `string.concat(a, b)` | `a` followed by `b` |
| `string.eq(a, b)` | `true` when `a` and `b` hold the same bytes |
| `string.byte_at(s, i)` | `StringError!u8`: the byte at index `i` |
| `string.slice(s, lo, hi)` | `StringError!String`: bytes `lo..hi` |

```vl
use std.string;

fun main() {
    val both = string.concat("hi", " there");
    val same = string.eq("VL", "VL");
}
```

Bounds-checked reads are fallible: out-of-range indices are typed errors,
not traps. Handle them with `catch`, propagate with `try`, or match on
the set:

```vl
use std;
use std.string;

fun main() {
    val byte = string.byte_at("hi", 9u64) catch 0u8;
    byte;
    match (string.slice("hello", 9u64, 10u64)) {
        std.string.StringError.OutOfBounds { std.print("too far\n"); }
        std.string.StringError.InvalidRange { std.print("backwards\n"); }
        else { std.print("ok\n"); }
    }
}
```

`string.to_u64` / `string.hex_to_u64` still trap on bad input (their VM
natives report no status yet); fallible parsing is a follow-up.

Numbers and strings are different types. To turn a number into text, use
`std.fmt`:

```vl
use std.fmt;

fun main() {
    val text = fmt.u64_to_string(42u64);
}
```

## Putting it together

```vl
use std;
use std.string;

fun greet(name: String) {
    val message = string.concat("Hello, ", name);
    std.println(message);
}

fun main() {
    greet("Ada");
}
```

Continue with [modules](/docs/modules).

---
layout: ../../layouts/Docs.astro
title: Modules and strings
description: Organize VL files with imports and work with strings and the standard library.
eyebrow: Learn the language
availability: VL 0.1+
---

# Modules and strings

As a program grows, put related code in separate files and reuse it with
imports. VL also provides a small standard library for common operations such
as printing (`std.println` appends a newline; `std.print` writes a string as-is).

## Source files are modules

Each `.vl` file is a module named after its filename without the extension. In
a project, the directory path is part of the module name. A `use` statement
makes another module available in the current file.

```vl
use std;

fun main() {
    std.println("hello");
}
```

Here, `std` is the module name and `println` is an export from that module. The
dotted form `std.println(...)` makes it clear where the function comes from.

## Importing a single export

A trailing name can be imported directly. This lets you call the function
without writing its module name:

```vl
use std.println;

fun main() {
    println("hello");
}
```

Grouped imports select several exports from one module:

```vl
use std.string.{len, eq};

fun main() {
    val same = eq("VL", "VL");
    val size = len("VL");
}
```

Unknown modules and exports are reported when you run `check`. The [standard
library catalog](/std) lists the modules and functions currently available.

## Sharing object types

Object types are nominal and cross modules by fully qualified name. A
`type Person` declared in module `my_app.person` is spelled
`my_app.person.Person` everywhere else: in function signatures, annotations,
and literals.

```vl
use my_app.person;

fun main() {
    var rose = person.new("Rose", 25);
    val same: my_app.person.Person = rose;
    val lit: my_app.person.Person = my_app.person.Person { name = "Lit", age = 40 };
}
```

Two modules may each declare their own `Person`; the qualified name keeps
them disjoint. Referring to a qualified type with no layout in scope is an
error, just like a misspelled bare type.

## The standard library

The standard library has two layers. Native functions in the target provide
low-level operations such as output, string conversion, and arithmetic. The
VL source modules in [`crates/vl-stdlib/src/std/`](https://github.com/Araozu/vl/tree/main/crates/vl-stdlib/src/std)
build small helpers on top of those natives. See the [standard library
reference](/std) for both layers and the source file for each module.

For example, `std.math` includes the VL helpers `max_u64`, `min_u64`,
`clamp_u64`, `max_i64`, `min_i64`, `abs_i64`, `is_even`, and generic `max`.

## Strings

Strings use double quotes. They are byte strings for now, which means they are
not yet a full Unicode text type. A string may contain escaped characters:

```vl
val greeting = "hello\nworld";
val quote = "say \"hi\"";
```

The supported escapes are `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`, and `\\"`.
Newlines and unescaped double quotes cannot appear inside a string literal.

The standard library provides functions that operate on strings:

```vl
use std.string;

fun main() {
    val message = "hello";
    val size = string.len(message);
    val empty = string.is_empty(message);
}
```

## Putting the pieces together

A small program can combine an import, a function, and a string:

```vl
use std;

fun greet(name: String) {
    std.println("Hello, ");
    std.println(name);
    std.println("!");
}

fun main() {
    greet("Ada");
}
```

When you are ready to explore available library functions, visit the [standard
library](/std). For compiler commands and diagnostics, see the [CLI
reference](/cli).

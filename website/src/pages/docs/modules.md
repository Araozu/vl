---
layout: ../../layouts/Docs.astro
title: Modules
description: Split VL programs across files and reuse code with imports.
eyebrow: Learn the language
availability: VL 0.1+
---

# Modules

As a program grows, put related code in separate files and reuse it with
imports. Each `.vl` file is one module.

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

An object type declared in another file keeps its full name everywhere else.
A `type Person` in module `my_app.person` is written
`my_app.person.Person` in annotations and literals:

```vl
use my_app.person;

fun main() {
    val lit: my_app.person.Person = my_app.person.Person { name = "Lit", age = 40 };
}
```

Writing the full name keeps same-named types from different files apart. A
misspelled module or export is reported when you run `check`.

## Importing a type by name

An object or union type can also be imported with braces. This brings the
bare name into scope for annotations, literals, and associated calls:

```vl
use my_app.person.{Person};

fun main() {
    val lit: Person = Person { name = "Lit", age = 40 };
}
```

The bare name still means the fully qualified type (`my_app.person.Person`
above), so nominal identity never collides: importing the same name from
two modules, or alongside a local type of the same name, is an error.

## The standard library

The standard library is a set of ready-made modules: `std` for printing,
`std.string` for text, `std.math` for numbers, `std.fmt` for turning
numbers into strings, `std.fs` for files, and `std.net.tcp` for TCP
sockets (`std.fs` and `std.net.tcp` are native linux-x86_64 only,
not available in the wasm32 playground). See the [standard library](/std) for the full list.

```vl
use std.math;

fun main() {
    val larger = math.max_u64(3u64, 8u64);
}
```

## Putting the pieces together

A small program combines an import, a function, and a string:

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

For text values and their helpers, see [strings](/docs/strings). For compiler
commands, see the [CLI reference](/cli).

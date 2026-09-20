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
as printing (`std.print` writes a string as-is; `std.println` appends a
newline).

## Source files are modules

Each `.vl` file is a module named after its filename without the extension. A
`use` statement makes another module available in the current file.

```vl
use std;

function main() {
    std.print("hello\n");
}
```

Here, `std` is the module name and `print` is an export from that module. The
dotted form `std.print(...)` makes it clear where the function comes from.

## Importing a single export

A trailing name can be imported directly. This lets you call the function
without writing its module name:

```vl
use std.print;

function main() {
    print("hello\n");
}
```

Grouped imports select several exports from one module:

```vl
use std.fs.{open, read};

function main() {
    let file = open("data.txt");
    let contents = read(file);
}
```

Unknown modules and exports are reported when you run `check`. The [standard
library catalog](/std) lists the modules and functions currently available.

## Strings

Strings use double quotes. They are byte strings for now, which means they are
not yet a full Unicode text type. A string may contain escaped characters:

```vl
let greeting = "hello\nworld";
let quote = "say \"hi\"";
```

The supported escapes are `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`, and `\\"`.
Newlines and unescaped double quotes cannot appear inside a string literal.

The standard library provides functions that operate on strings:

```vl
use std.string;

function main() {
    let message = "hello";
    let size = string.len(message);
}
```

## Putting the pieces together

A small program can combine an import, a function, and a string:

```vl
use std;

function greet(name: String) {
    std.print("Hello, ");
    std.print(name);
    std.print("!\n");
}

function main() {
    greet("Ada");
}
```

When you are ready to explore available library functions, visit the [standard
library](/std). For compiler commands and diagnostics, see the [CLI
reference](/cli).

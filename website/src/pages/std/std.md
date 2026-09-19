---
layout: ../../layouts/Docs.astro
title: std module
description: Standard output functions for VL programs.
eyebrow: Standard library
availability: VL 0.1+
---

# `std`

The `std` module contains the output functions used by the getting-started
example.

Import the module before calling it:

```vl
use std.print;

function main() {
    print("Hello, world!\n");
}
```

## `std.print(value)`

Writes one byte string to standard output. It does not add an implicit newline.

```vl
print("ready\n");
```

`std.print` is supported by the Naravm target and currently expects exactly one
string argument.

## `std.print_u64(value)`

The module catalog reserves `print_u64` for unsigned integer output. It is
recognized during general module checking, but the current Naravm target does
not emit it yet.

For the current target and command options, see the [CLI reference](/cli).

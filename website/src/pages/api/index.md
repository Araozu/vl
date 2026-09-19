---
layout: ../../layouts/Docs.astro
title: Standard library API
description: The modules and functions available to VL programs.
eyebrow: Reference
availability: VL 0.1+
---

# Standard library API

The standard library is the set of modules a VL program can import. Start with
the [learning path](/docs) if you are new to the language, then use these pages
to look up a module or function.

## Modules

### [`std`](/api/std)

Output functions for small command-line programs.

### [`std.fs`](/api/fs)

File-system functions recognized by the language module catalog. Target support
is still in progress.

### [`std.string`](/api/string)

String helpers recognized by the language module catalog. Target support is
still in progress.

## Availability

VL 0.1 is being implemented in stages. A function may be accepted by the
language checker before a runnable target supports it. Each module page calls
out the current status; `std.print` is the current Naravm example.

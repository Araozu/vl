---
layout: ../../layouts/Docs.astro
title: std.string module
description: String functions in the VL standard library catalog.
eyebrow: Standard library
availability: VL 0.1+
---

# `std.string`

The `std.string` module reserves helpers for creating and measuring byte
strings. Import the functions you need:

```vl
use std.string.{new, len};
```

## `std.string.new`

Creates a string value. The function is present in the language module
catalog; its target-specific argument and result behavior is still being
defined.

## `std.string.len`

Returns the length of a string. The function is present in the language module
catalog; the current Naravm target does not emit it yet.

VL strings are byte strings for now. See the [language guide](/docs/language)
for literal syntax and supported escapes.

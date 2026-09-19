---
layout: ../../layouts/Docs.astro
title: std.fs module
description: File-system functions in the VL standard library catalog.
eyebrow: Standard library
availability: VL 0.1+
---

# `std.fs`

The `std.fs` module reserves the file-system functions available to the
language. Import individual functions with a grouped import:

```vl
use std.fs.{open, read};
```

## `std.fs.open`

Opens a file. The callable surface is present in the module catalog; the
current Naravm target does not emit file-system calls yet.

## `std.fs.read`

Reads from an opened file. Like `open`, this function is currently a checked
language surface rather than a runnable Naravm operation.

When target support lands, this page will document argument and result types.

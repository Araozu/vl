---
layout: ../../layouts/Docs.astro
title: Errors
description: Declare error sets, return fallible values, and handle them with try and catch.
eyebrow: Learn the language
availability: VL 0.1+
---

# Errors

Some operations can fail: reading a file, accessing a string byte outside
its bounds, or talking to a socket. VL models failure with error sets, in the
style of Zig.

Declare a set with `type` and `error`, listing the ways it can fail. Variant
names start with an uppercase letter, and the declaration ends with a
semicolon. Variants take optional union-style payloads:

```vl
type Io = error { NotFound, Denied, Missing(String), };
```

An error value names the set and the variant together:

```vl
type Io = error { NotFound, Denied, };

fun missing(): Io {
    return Io.NotFound;
}
```

Payload constructions (`Io.Missing(path)`) only carry data through fallible
values (below): a plain `Io` holds just the error code, so building one
with arguments is an error.

## Fallible functions

A function that can fail says so in its return type: `Io!u64` is a `u64` or
an `Io` error. Write `!u64` when any error set will do:

```vl
type Io = error { NotFound, };

fun read_byte(ok: bool): Io!u64 {
    if (ok) {
        return 42u64;
    }
    return Io.NotFound;
}
```

A plain `u64` value wraps as success — and an `Io` value as the error —
wherever `Io!u64` is expected. `Io!void` marks a fallible side effect
(a bare `return;` succeeds).

## Try: propagate

`try` unwraps a fallible value. When it holds an error, the error returns
from the enclosing function at once — so the enclosing function must be
fallible too:

```vl
type Io = error { NotFound, };

fun read_two(a: bool, b: bool): Io!u64 {
    val x = try read_byte(a);
    val y = try read_byte(b);
    return x + y;
}
```

## Catch: handle

`catch` supplies a fallback for the error case and always produces a plain
value:

```vl
fun main() {
    val first = read_byte(true) catch 0u64;
    val second = read_byte(false) catch 0u64;
}
```

Using a fallible value without `try` or `catch` — passing it on, dropping
it, or branching on it — is an error. The entrypoint itself stays
infallible: handle errors inside `main` with `catch`.

## Match: read payloads

`match` reads error values back. Over a plain set it dispatches on the
code (payload patterns bind nothing there — a bare pattern matches the
code). Over a fallible, listed arms bind error payloads while `else`
covers the ok value plus unlisted variants:

```vl
use std;

type Io = error { NotFound, Missing(String), };

fun report(r: Io!u64) {
    match (r) {
        Io.Missing(path) { std.print(path); }
        Io.NotFound { std.print("gone\n"); }
        else { std.print("ok\n"); }
    }
}
```

`try` forwards error payloads untouched, so a helper can propagate and its
caller still match on the data. `catch` discards the error (payloads
included) and evaluates its fallback.

## Importing error sets

Error sets cross modules like other named types. A provider declares the
set; importers name it qualified or bring it into scope:

```vl
use demo.io;

fun main() {
    val e: demo.io.Io = demo.io.Io.Denied;
}
```

```vl
use demo.io.{Io};

fun strict(): Io!u64 {
    return Io.NotFound;
}
```

Continue with [strings](/docs/strings).

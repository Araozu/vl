---
layout: ../../layouts/Docs.astro
title: Basics
description: Learn the shape of a VL program, values, types, variables, and operators.
eyebrow: Learn the language
availability: VL 0.1+
---

# Basics

This page introduces the pieces that appear in almost every VL program. VL is
small and deliberately familiar: it uses braces for blocks, `var` and `val`
for bindings, `fun` for reusable work, and semicolons to finish statements.

## Your first program

```vl
use std;

fun main() {
    std.println("Hello, VL!");
}
```

Read this from top to bottom:

1. `use std;` makes the standard library module available.
2. `fun main()` defines the starting point of the program.
3. Braces mark the function body.
4. `std.println(...)` calls a function.
5. The semicolon marks the end of the call.

`std.println` writes the string and appends a `"\n"` for you. A plain
`std.print` also exists and writes the string exactly as given.

Every runnable program needs a zero-argument `fun main()` that returns
`void`. A missing return type means `void`, so `fun main()` and
`fun main(): void` mean the same thing.

## Comments

Comments are notes for people. VL ignores them when it compiles the program.

```vl
// This is a whole-line comment.
val answer = 40 + 2; // This comment starts after the statement.
```

## Values and types

A value is a piece of data, such as the number `42` or the word `"hello"`.
Every value has a type. The type tells VL what kind of data it is and which
operations make sense for it.

| Type | Example | Use it for |
| --- | --- | --- |
| `u64` | `42` | A non-negative whole number |
| `i64` | `-42i64` | A whole number that can be negative |
| `u8` | `255u8` | A small non-negative number |
| `f64` | `3.14f64` | A decimal number |
| `bool` | `true` | A yes/no value |
| `String` | `"hello"` | Text |
| `object` type | `Counter { value = 1 }` | Your own named data (see [Objects](/docs/objects)) |

You will also see a `*` in front of some types, as in `*Counter` or
`*Array[u64]`. The `*` means "allowed to change the contents". A plain
`Counter` can be read; a `*Counter` can also be written. The
[Objects](/docs/objects) and [Arrays](/docs/arrays) chapters explain when to
use each form.

Integer literals are chosen from their context. For example, the parameter
type tells VL what type the `1` and `2` should have here:

```vl
fun add(a: i64, b: i64): i64 {
    return a + b;
}

fun main() {
    val total = add(1, 2);
}
```

Decimal literals need an explicit suffix such as `f64`. VL does not silently
mix numeric types: a `u64` variable cannot be used where a `u8` is expected.
Plain integer literals like `1` adapt to whatever the context needs
(`val x: u8 = 3;` checks that `3` fits).

Explicit conversions use `as` (integers only):

```vl
val v = 200u64;
val w = v as u8;
val lit = 10 as u8;
```

Out-of-range literals such as `300 as u8` are rejected when compiling.

## Bindings with `var` and `val`

`var` creates a name you can reassign. `val` creates a name that stays fixed.

```vl
val name = "Ada";
var visits = 1;
val welcome = visits == 1;
```

A type can also be written down explicitly. The value must have that type
(plain integer literals adapt, so `3` works where `u64` is written):

```vl
val retries: u64 = 3;
var scores: *Array[u64] = Array.new(3);
```

The `*` in `*Array[u64]` means the array contents may be changed. As a rule
of thumb: use `var` when a fresh object or array should be changeable, and
`val` when it should stay as created. Writing the type explicitly always
wins over the default. The [Objects](/docs/objects) and
[Arrays](/docs/arrays) chapters show this in detail.

Named object types use `type Name = object { ... };` declarations. Their fields
are comma-separated, and a literal fills in every field:

```vl
type Point = object { x: u64, y: u64, };

fun main() {
    var point = Point { x = 10, y = 20 };
    point.x = 11;
}
```

Objects share rather than copy: passing an object to a function hands over
the same object, so a change made through one name is visible through the
others. See the [Objects](/docs/objects) chapter for details.

A `var` name can be assigned a new value later:

```vl
type Counter = object { value: u64, };

fun main() {
    var count = 0;
    count = count + 1;
    var current = Counter { value = 0 };
    current = Counter { value = 10 };
}
```

A variable only exists in the scope where it was declared. A name declared
inside a function is not available at the top level or inside another
function.

## Operators

Arithmetic operators work on numbers: `+`, `-`, `*`, and `/`. Parentheses make
the order explicit, just as they do in mathematics.

```vl
val first = 1u64 + 2u64 * 3u64;       // 7
val second = (1u64 + 2u64) * 3u64;    // 9
val third = -second;
```

Comparisons produce a `bool`:

```vl
val age = 21u64;
val left = 1u64;
val right = 2u64;
val old_enough = age >= 18u64;
val same = left == right;
val different = left != right;
```

Boolean values can be combined with `&&` (and), `||` (or), and `!` (not).
They are evaluated from left to right, and `&&` and `||` stop early when the
answer is already known.

## Statements and semicolons

VL requires semicolons after declarations, assignments, returns, calls, and
other statements. A function body is a list of statements inside braces.

```vl
use std;

fun main() {
    val message = "ready";
    std.println(message);
}
```

There are no implicit returns: a function only produces a result when it uses
an explicit `return value;`. The next page explains how to make a program
choose between statements and repeat them.

Continue with [conditions and loops](/docs/control-flow).

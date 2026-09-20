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
| `String` | `"hello"` | A byte string (read-only view) |
| `object` type | `Counter { value = 1 }` | Named GC data; `Foo` reads, `*Foo` mutates |

`String` and `File` are reference types too, so `*String` and `*File` carry
the mutable capability when their APIs expose mutable operations. A plain
`String` or `File` is the read-only view.

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

Decimal literals need an explicit suffix such as `f64`. Values do not silently
change from one numeric type to another; when a type matters, write it down.
Integer literals adapt to their context (`val x: u8 = 3;` checks the range),
but variables never convert implicitly: a `u64` variable does not flow into a
`u8` parameter.

Explicit conversions use `as` (integers only in v0):

```vl
val v = 200u64;
val w = v as u8;
val lit = 10 as u8;
```

Literals are range-checked at compile time (`300 as u8` fails); variable
conversions are unchecked reinterpretations with no runtime cost (no trap, no
wrap instruction).

## Bindings with `var` and `val`

`var` creates a rebindable binding. `val` creates a fixed binding. Both can
hold primitives or references, and the binding keyword is independent from the
reference capability: `Foo` is a read-only view of a GC-managed `Foo`, while
`*Foo` is a mutable view of the same allocation. Passing or assigning either
spelling copies the GC reference.

For a reference created directly in an unannotated `var`, VL infers the
mutable capability. An unannotated `val` keeps the read-only view. An explicit
annotation always wins, so `var view: Foo = ...` is rebindable but read-only,
and `val editable: *Foo = ...` is fixed but can mutate its referent.

```vl
val name = "Ada";
var visits = 1;
val welcome = visits == 1;
```

A type can also be written down explicitly with an annotation. The value must
have that type (integer literals adapt, so `3` works where `u64` is written).
An annotation is required to give `Array.new` its element type without a
turbofish. Fresh reference data adopts an expected `*` capability:

```vl
type Foo = object { value: u64, };

val retries: u64 = 3;
var scores: *Array[u64] = Array.new(3);
val view = Foo { value = 0 };
var editable = Foo { value = 0 }; // inferred *Foo
val fixed_editable: *Foo = Foo { value = 0 }; // fixed binding, mutable view
```

Named object types use `type Name = object { ... };` declarations. Their fields
are comma-separated, and an object literal initializes every field:

```vl
type Point = object { x: u64, y: u64, };

fun main() {
    var point = Point { x = 10, y = 20 }; // inferred *Point
    point.x = 11;
}
```

Objects have reference semantics: assignment and function calls share the same
heap object, so a field write through a `*Point` is visible through every
alias, including read-only ones. See the
[Objects](/docs/objects) chapter for details.

The name can be assigned a new value later, but the replacement must be
coercible to the declared type: a `*Foo` value downgrades into a `Foo`
binding, while a `Foo` value never upgrades into a `*Foo` binding (a
`*Foo` binding accepts a fresh `Foo {}` via contextual capability):

```vl
type Counter = object { value: u64, };

fun main() {
    var count = 0;
    count = count + 1;
    var current = Counter { value = 0 }; // inferred *Counter
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

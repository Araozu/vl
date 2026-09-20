---
layout: ../../layouts/Docs.astro
title: Basics
description: Learn the shape of a VL program, values, types, variables, and operators.
eyebrow: Learn the language
availability: VL 0.1+
---

# Basics

This page introduces the pieces that appear in almost every VL program. VL is
small and deliberately familiar: it uses braces for blocks, `let` for
variables, `function` for reusable work, and semicolons to finish statements.

## Your first program

```vl
use std;

function main() {
    std.print("Hello, VL!\n");
}
```

Read this from top to bottom:

1. `use std;` makes the standard library module available.
2. `function main()` defines the starting point of the program.
3. Braces mark the function body.
4. `std.print(...)` calls a function.
5. The semicolon marks the end of the call.

`std.print` writes the string exactly as given. When you want the line to
end with a newline, call `std.println("Hello, VL!")` instead — it appends
`"\n"` for you.

Every runnable program needs a zero-argument `function main()` that returns
`void`. A missing return type means `void`, so `function main()` and
`function main(): void` mean the same thing.

## Comments

Comments are notes for people. VL ignores them when it compiles the program.

```vl
// This is a whole-line comment.
let answer = 40 + 2; // This comment starts after the statement.
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
| `string` | `"hello"` | A byte string |
| `object` type | `Counter { value: 1 }` | Named mutable data with reference semantics |

Integer literals are chosen from their context. For example, the parameter
type tells VL what type the `1` and `2` should have here:

```vl
function add(a: i64, b: i64): i64 {
    return a + b;
}

function main() {
    let total = add(1, 2);
}
```

Decimal literals need an explicit suffix such as `f64`. Values do not silently
change from one numeric type to another; when a type matters, write it down.
Integer literals adapt to their context (`let x: u8 = 3;` checks the range),
but variables never convert implicitly: a `u64` variable does not flow into a
`u8` parameter.

Explicit conversions use TypeScript-like `as` (integers only in v0):

```vl
let v = 200u64;
let w = v as u8;
let lit = 10 as u8;
```

Literals are range-checked at compile time (`300 as u8` fails); variable
conversions are unchecked reinterpretations with no runtime cost (no trap, no
wrap instruction).

## Variables with `let`

`let` gives a value a name so you can use it later. VL infers the type from the
value on the right side of `=`.

```vl
let name = "Ada";
let visits = 1;
let welcome = visits == 1;
```

A type can also be written down explicitly with an annotation. The value must
have that type (integer literals adapt, so `3` works where `u64` is written).
An annotation is required to give `Array.new` its element type without a
turbofish:

```vl
let retries: u64 = 3;
let scores: Array[u64] = Array.new(3);
```

Named object types use `type Name = object { ... };` declarations. Their fields
are comma-separated, and an object literal initializes every field:

```vl
type Point = object { x: u64, y: u64, };
let point = Point { x: 10, y: 20 };
point.x = 11;
```

Objects have reference semantics: assignment and function calls share the same
heap object, so a field write is visible through every alias. See the
[Objects](/docs/objects) chapter for details.

The name can be assigned a new value later, but the replacement must have the
same type:

```vl
function main() {
    let count = 0;
    count = count + 1;
}
```

A variable only exists in the scope where it was declared. A name declared
inside a function is not available at the top level or inside another
function.

## Operators

Arithmetic operators work on numbers: `+`, `-`, `*`, and `/`. Parentheses make
the order explicit, just as they do in mathematics.

```vl
let first = 1 + 2 * 3;       // 7
let second = (1 + 2) * 3;    // 9
let third = -second;
```

Comparisons produce a `bool`:

```vl
let old_enough = age >= 18;
let same = left == right;
let different = left != right;
```

Boolean values can be combined with `&&` (and), `||` (or), and `!` (not).
They are evaluated from left to right, and `&&` and `||` stop early when the
answer is already known.

## Statements and semicolons

VL requires semicolons after declarations, assignments, returns, calls, and
other statements. A function body is a list of statements inside braces.

```vl
use std;

function main() {
    let message = "ready";
    std.print(message);
}
```

There are no implicit returns: a function only produces a result when it uses
an explicit `return value;`. The next page explains how to make a program
choose between statements and repeat them.

Continue with [conditions and loops](/docs/control-flow).

---
layout: ../../layouts/Docs.astro
title: Unions
description: Model either-or values with union types, build variants, and match on them.
eyebrow: Learn the language
availability: VL 0.1+
---

# Unions

An object groups fields that are always present together. A union is the
opposite: a value that is exactly one of several options.

Think of a traffic light. It is red, yellow, or green — never two at once:

```vl
type TrafficLight = union { Red, Yellow, Green, };

fun main() {
    val light = TrafficLight.Green;
}
```

Each option is called a variant. Variant names start with an uppercase
letter, and the declaration ends with a semicolon.

## Variants with data

A variant can carry values inside parentheses. A circle holds a radius, a
rectangle holds a width and height, and a dot holds nothing:

```vl
type Shape = union { Circle(f64), Rect(f64, f64), Dot, };
```

Build a value by naming the union and the variant together:

```vl
type Shape = union { Circle(f64), Rect(f64, f64), Dot, };

fun main() {
    val a = Shape.Circle(1.0f64);
    val b = Shape.Rect(3.0f64, 4.0f64);
    val c = Shape.Dot;
}
```

A variant with parentheses needs its values (`Shape.Circle(1.0f64)`), and a
variant without them stands alone (`Shape.Dot`). Writing `Shape { ... }`
like an object literal does not work — always go through a variant.

## Choosing with `match`

Use `match` to ask which variant a value holds and pull out its data:

```vl
use std;

type Shape = union { Circle(f64), Rect(f64, f64), Dot, };

fun main() {
    val s = Shape.Circle(1.0f64);
    match (s) {
        Shape.Circle(r) {
            std.print("circle\n");
        }
        Shape.Rect(w, h) {
            std.print("rect\n");
        }
        Shape.Dot {
            std.print("dot\n");
        }
    }
}
```

Each arm names one `Union.Variant`, optionally followed by names for the
carried values in parentheses. Those names are fresh fixed bindings you can
use inside the arm's braces.

`match` must cover every variant. Either list all of them, as above, or add
an `else` arm at the end for the rest (inside a `match` on `s`):

```vl
match (s) {
    Shape.Circle(r) {
        std.print("circle\n");
    }
    else {
        std.print("something else\n");
    }
}
```

A `match` without full coverage is an error, and `else` must come last.

## A reusable union: `Option`

Unions can take type parameters, just like generic functions. `Option` is
the classic example — a value that is either present or missing:

```vl
use std;

type Option[T] = union { None, Some(T), };

fun main() {
    val present = Option.Some(42u64);
    val missing: Option[u64] = Option.None;
    match (present) {
        Option.Some(v) {
            std.print_u64(v);
        }
        else {
            std.print("nothing\n");
        }
    }
}
```

VL usually figures out `T` from the carried value (`Option.Some(42u64)` is
an `Option[u64]`). A bare `None` carries nothing to guess from, so annotate
it: `val missing: Option[u64] = Option.None;`.

Union values have reference semantics, like objects: assignment, parameters,
and returns alias the same heap value. Payloads carried by value (numbers,
tuples) are copied into the variant on construction, so later writes through
the original binding never affect the constructed value.

The same rules apply everywhere else: variant names are uppercase,
payloads cannot be empty (`Some()` is rejected), and matching checks the
number of bindings per arm.

Continue with [strings](/docs/strings).

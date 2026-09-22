---
layout: ../../layouts/Docs.astro
title: Arrays and generics
description: Store groups of values in arrays and write reusable generic functions in VL.
eyebrow: Learn the language
availability: VL 0.1+
---

# Arrays and generics

An array stores several values of one type. VL arrays have a fixed length:
the length is chosen when the array is created and cannot grow afterwards.

## Creating an array

An array literal uses square brackets. All its elements must have the same
type. Use `var` when the new array should be changeable and `val` when it
should stay as created:

```vl
val scores = [10, 20, 30];
val words = ["one", "two"];
```

For an array whose size is known but whose values will be filled in later,
use the builtin `Array.new` constructor. Reading works through either form;
writing needs a writable `*Array[T]` name:

```vl
fun main() {
    var scores: *Array[u64] = Array.new(3);
    scores[0] = 10;
    scores[1] = 20;
    scores[2] = 30;
}
```

You can also write the element type at the call with `::[...]`:

```vl
val scores = Array.new::[u64](3);
```

The new array is filled with zero values. An empty literal `[]` has no element
type for VL to infer, so annotate it (`val e: Array[u64] = [];`) or use
`Array.new::[T](n)` when the array starts empty.

## Reading and writing elements

An index selects one element. Indexes start at zero, so the first element is
`values[0]` and the last element of a three-element array is `values[2]`.

```vl
fun main() {
    val values: *Array[u64] = [4, 8, 15];
    val first = values[0];
    values[1] = first + 1;
}
```

Indexes are `u64` values. Reading or writing outside the array bounds stops
the program, so keep the length alongside the array when a loop needs it. A
writable `*Array[T]` can be used where a read-only `Array[T]` is expected,
never the other way around.

```vl
fun sum(values: Array[u64], count: u64): u64 {
    var total = 0;
    var i = 0;
    while (i < count) {
        total = total + values[i];
        i = i + 1;
    }
    return total;
}
```

```vl
fun fill(values: *Array[u64]) {
    values[0] = 1;
}
```

Arrays share rather than copy. Passing an array to a function hands over the
same array, so an element write through a writable name changes the shared
array.

## Generic functions

Sometimes an algorithm does not care what the element type is. A generic type
parameter is a placeholder for a real type chosen at the call site.

`first[T]` below works for an array of any `T`:

```vl
fun first[T](values: Array[T]): T {
    return values[0];
}

fun main() {
    val numbers: *Array[u64] = [10, 20];
    val number = first(numbers);
    val words = ["hi", "bye"];
    val word = first(words);
}
```

VL infers `T` from the argument: it chooses `u64` for `numbers` and `String`
for `words`. If VL cannot figure it out, write the type in `::[...]`:

```vl
fun first[T](values: Array[T]): T {
    return values[0];
}

fun main() {
    val numbers: *Array[u64] = [10, 20];
    val number = first::[u64](numbers);
}

```

The `::` matters. `first[T](...)` without it means something else, not a
generic call.

Each concrete use of a generic function gets its own compiled copy. A
generic function that is never called produces no copy, and `main` itself
cannot be generic.

## Constrained generics

An unconstrained `T` can only be moved around, not computed with. Bounds
unlock operators for known families of types:

```vl
fun add[T extends Numeric](a: T, b: T): T {
    return a + b;
}

fun eq[T extends Comparable](a: T, b: T): bool {
    return a == b;
}
```

`Numeric` allows arithmetic (`+ - * /`), ordering, and equality over
`u64`, `i64`, `f64`, and `u8`. `Comparable` allows equality (`== !=`) over
numbers, `bool`, and `String`. A `Numeric` value can be used where
`Comparable` is expected, but an unconstrained `T` fits neither.

Continue with [objects](/docs/objects).

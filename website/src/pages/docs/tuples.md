---
layout: ../../layouts/Docs.astro
title: Tuples
description: Group fixed sets of differently typed values with named or positional tuples.
eyebrow: Learn the language
availability: VL 0.1+
---

# Tuples

A tuple groups a fixed number of values, which may have different types. Use
an unnamed tuple when each position is enough, or give each element a name
when the names make the data easier to read.

## Unnamed tuples

Write tuple types with `#(...)` and tuple values with the same marker. Tuples
need at least two elements:

```vl
val point: #(u64, u64) = #(3u64, 4u64);
val x = point.`0;
val y = point.`1;
```

Unnamed positions use the backtick index form, starting at zero. The type and
value must have the same number of elements, and each position has its own
type.

## Named tuples

Name every element in both the type and the value. Read a named element with
dot access:

```vl
val person: #(name: String, age: u64) = #(name = "Ada", age = 36u64);
val name = person.name;
val age = person.age;
```

The names and order in the value must match the tuple type. A tuple cannot
mix named and unnamed elements.

## Destructure a tuple

Destructuring binds several tuple elements at once. Unnamed tuples bind by
position; named tuples spell the field before the local name:

```vl
fun main() {
    val pair = #(10u64, "ten");
    val #(number, label) = pair;

    val user = #(name = "Ada", age = 36u64);
    val #(name: user_name, age: user_age) = user;
}
```

You can also reassign elements through a writable tuple view. A whole tuple
is copied when it is assigned to another name, so changing one tuple does not
change the other tuple:

```vl
fun main() {
    var pair: *#(u64, u64) = #(1u64, 2u64);
    pair.`0 = 10u64;

    var named: *#(x: u64, y: u64) = #(x = 3u64, y = 4u64);
    named.x = 30u64;
}
```

Tuples work with function parameters and returns just like other types. They
are also useful when an operation naturally produces multiple results, such
as the socket and port from `tcp.listen`. See [functions](/docs/functions),
[errors](/docs/errors), and the [standard library](/std).

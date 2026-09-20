---
layout: ../../layouts/Docs.astro
title: Objects
description: Define named object types, initialize their fields, and work with reference semantics.
eyebrow: Learn the language
availability: VL 0.1+
---

# Objects

An object is a named collection of fields. Declare one with `type`, followed
by its name and an `object` body. Fields are comma-separated; the declaration
ends with a semicolon.

```vl
type Counter = object {
    value: u64,
    label: string,
};
```

Create an object with a named literal. Every declared field must appear exactly
once, and fields may be written in any order:

```vl
function main() {
    let counter = Counter { label: "count", value: 0 };
    counter.value = counter.value + 1;
}
```

## Reference semantics

Objects are reference values. Assigning an object, passing it to a function,
or returning it passes the same object; it does not copy the fields. A field
write is therefore visible through every alias:

```vl
function bump(counter: Counter): Counter {
    counter.value = counter.value + 1;
    return counter;
}

function main() {
    let first = Counter { value: 1, label: "count" };
    let second = bump(first);
    second.value = second.value + 1;
    // first.value is now 3.
}
```

Object fields use the normal VL types, including `string`, `Array[T]`, and
other object types. A field whose type is itself a reference value keeps that
reference when the containing object is assigned. Array elements remain
mutable through their existing indexing operations.

Objects are nominal data types. VL currently gives them fields and reference
semantics only: there are no implicit constructors, methods, inheritance,
runtime casts, or identity/equality operators for objects.

Continue with [modules and strings](/docs/modules).

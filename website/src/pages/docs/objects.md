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
    label: String,
};
```

Create an object with a named literal. Every declared field must appear exactly
once, and fields may be written in any order. Declarations use `name: type`,
while literals assign with `name = value`. A fresh literal adopts an expected
`*` capability and otherwise defaults to a read-only view:

```vl
type Counter = object { value: u64, label: String, };

function main() {
    let counter: *Counter = Counter { label = "count", value = 0 };
    counter.value = counter.value + 1;
}
```

## Read-only views, mutable views, and aliasing

`Foo` is a read-only view of a GC-managed `Foo`; `*Foo` is a mutable view of
the same kind of allocation. Mutation authority belongs to each view, not to
the heap object: there is no freezing, no borrow checker, and no exclusive
mutable alias. Multiple `*Foo` aliases may coexist with read-only ones, and a
read-only alias observes writes made through a mutable one.

```vl
type Counter = object { value: u64, label: String, };

function bump(counter: *Counter): *Counter {
    counter.value = counter.value + 1;
    return counter;
}

function main() {
    let editable: *Counter = Counter { value = 1, label = "count" };
    let view: Counter = editable; // allowed downgrade

    editable.value = 2; // visible through `view`
    let same = bump(editable);
    same.value = same.value + 1;
    // editable.value is now 4.
}
```

## Reference semantics

Objects are reference values. Assigning an object, passing it to a function,
or returning it passes the same object; it does not copy the fields. A field
write through a mutable view is therefore visible through every alias:

```vl
type Counter = object { value: u64, label: String, };

function bump(counter: *Counter): *Counter {
    counter.value = counter.value + 1;
    return counter;
}

function main() {
    let first: *Counter = Counter { value = 1, label = "count" };
    let second = bump(first);
    second.value = second.value + 1;
    // first.value is now 3.
}
```

Field declarations carry the maximum stored capability. Reading through a
read-only receiver downgrades mutable fields transitively; a mutable receiver
preserves them. Field assignment itself requires a `*Object` receiver:

```vl
type Child = object { value: u64, };
type Parent = object {
    child: *Child,
    children: *Array[*Child],
};

function inspect(parent: Parent) {
    // parent.child.value = 1; // error: read-only view
}

function edit(parent: *Parent) {
    parent.child.value = 1; // allowed
}
```

Object fields use the normal VL types, including `String`, `Array[T]`,
`*Array[T]`, and other object types (including `*Child`). A field whose type
is itself a reference value keeps that reference when the containing object
is assigned. Array elements are read through either capability but written
only through a mutable `*Array[T]` access path (projected transitively, as
above).

Objects are nominal data types. VL currently gives them fields and reference
semantics only: there are no implicit constructors, methods, inheritance,
runtime casts, or identity/equality operators for objects.

Continue with [modules and strings](/docs/modules).

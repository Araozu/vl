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

fun main() {
    var counter = Counter { label = "count", value = 0 };
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

fun bump(counter: *Counter): *Counter {
    counter.value = counter.value + 1;
    return counter;
}

fun main() {
    val editable: *Counter = Counter { value = 1, label = "count" };
    val view: Counter = editable; // allowed downgrade

    editable.value = 2; // visible through `view`
    var same = bump(editable);
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

fun bump(counter: *Counter): *Counter {
    counter.value = counter.value + 1;
    return counter;
}

fun main() {
    val first: *Counter = Counter { value = 1, label = "count" };
    var second = bump(first);
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

fun inspect(parent: Parent) {
    // parent.child.value = 1; // error: read-only view
}

fun edit(parent: *Parent) {
    parent.child.value = 1; // allowed
}
```

Object fields use the normal VL types, including `String`, `Array[T]`,
`*Array[T]`, and other object types (including `*Child`). A field whose type
is itself a reference value keeps that reference when the containing object
is assigned. Array elements are read through either capability but written
only through a mutable `*Array[T]` access path (projected transitively, as
above).

Objects are nominal data types. Besides fields and reference semantics, VL
gives objects Zig-style associated functions declared inside the body. There
is no inheritance, no implicit constructor, and no runtime type reflection.

## Associated functions

Declare a `fun` member inside the `object` body. Fields and functions share
one member namespace, so a duplicate member name is an error. The comma after
a field may be omitted before a `fun` member; a trailing comma after a `fun`
member is allowed but never required.

```vl
type Counter = object {
    value: u64,

    fun init(v: u64): *Counter {
        return Counter { value = v };
    },

    fun bump(self: *Counter): *Counter {
        self.value = self.value + 1;
        return self;
    },

    fun get(self: Counter): u64 {
        return self.value;
    },
};
```

Calls spell the owner explicitly, exactly like the `Array.new` builtin:

```vl
fun main() {
    var counter = Counter.init(1);
    var same = Counter.bump(counter);
}
```

Nominal identity is spelling-sensitive, as for every other call: a value
annotated `Counter` and a value annotated `my.mod.Counter` do not coerce into
each other even inside the defining module, so spell the type the same way on
both sides of a call.

There is no implicit `self`: the receiver is an ordinary first parameter.
Readers take `self: Counter`; writers take `self: *Counter`, and the usual
capability rules apply (a read-only view never upgrades to `*Counter`).

## Instance sugar

When — and only when — the first parameter takes the receiver's object type,
the call may spell the receiver first: `counter.bump()` is sugar for
`Counter.bump(counter)`. A method whose first parameter is some other type
(or takes no parameters at all) can only be called through the type.

```vl
fun main() {
    var counter = Counter.init(1);
    counter.bump(); // Counter.bump(counter)
    counter.get();  // Counter.get(counter)
}
```

Associated functions may declare their own type parameters and are
monomorphized per call like free generic functions
(`box.wrap(7)` infers `Box.wrap$u64`). Across modules they live in the type
namespace: with `use vl.person;` in scope, call `person.Person.birthday(rose)`
or the fully qualified `vl.person.Person.birthday(rose)`; sugar on a foreign
value (`rose.birthday()`) resolves through the receiver's type and needs no
import. Cross-module calls meet the same module-global boundary as free
functions.

Continue with [modules and strings](/docs/modules).

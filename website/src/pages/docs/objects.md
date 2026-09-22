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
while literals assign with `name = value`:

```vl
type Counter = object { value: u64, label: String, };

fun main() {
    var counter = Counter { label = "count", value = 0 };
    counter.value = counter.value + 1;
}
```

Use `var` when the new object should be changeable and `val` when it should
stay as created. Writing the type explicitly (`val c: Counter = ...` or
`val c: *Counter = ...`) always wins over the default.

## Read-only views and writable views

`Counter` lets you read an object; `*Counter` (with a star) additionally
lets you change its fields. Several names may point at the same object, and
a change made through a writable name is visible through the others:

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

## Sharing objects

Objects share rather than copy. Assigning an object, passing it to a function,
or returning it hands over the same object. A field write through a writable
view is therefore visible through every other name for it:

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

Field assignment needs a writable (`*Object`) name:

```vl
type Child = object { value: u64, };
type Parent = object {
    child: *Child,
    children: *Array[*Child],
};

fun inspect(parent: Parent) {
    // parent.child.value = 1; // error: needs a writable view
}

fun edit(parent: *Parent) {
    parent.child.value = 1; // allowed
}
```

Object fields use the normal VL types, including `String`, `Array[T]`,
`*Array[T]`, and other object types. An array element can be read through
either form but written only through a writable `*Array[T]` path.

Each object type is distinct by name: two types with the same fields but
different names are different types. Objects can also own functions declared
inside the body (see below). There is no inheritance and no automatic
constructor.

## Associated functions

Declare a `fun` member inside the `object` body. A field and a function may
not share a name. The comma after a field may be omitted before a `fun`
member.

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

Call it through the type, just like the `Array.new` builtin:

```vl
fun main() {
    var counter = Counter.init(1);
    var same = Counter.bump(counter);
}
```

There is no hidden `self`: the receiver is an ordinary first parameter.
Readers take `self: Counter`; writers take `self: *Counter`.

## Instance sugar

When the first parameter takes the receiver's own object type, the call may
put the receiver first: `counter.bump()` means `Counter.bump(counter)`. A
function whose first parameter is some other type can only be called through
the type itself.

```vl
fun main() {
    var counter = Counter.init(1);
    counter.bump(); // Counter.bump(counter)
    counter.get();  // Counter.get(counter)
}
```

Functions on objects may use their own type parameters, just like free
generic functions. Across files, call them through the type
(`person.Person.birthday(rose)`); the short receiver form (`rose.birthday()`)
works from the value alone and needs no import.

Continue with [unions](/docs/unions).

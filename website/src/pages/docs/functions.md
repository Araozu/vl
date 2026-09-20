---
layout: ../../layouts/Docs.astro
title: Functions
description: Define reusable work, pass values to functions, and return results in VL.
eyebrow: Learn the language
availability: VL 0.1+
---

# Functions

A function is a named piece of work. Functions help you avoid repeating code
and give a large program smaller parts that are easier to understand.

## Define and call a function

This function receives two numbers and returns their sum:

```vl
function add(a: i64, b: i64): i64 {
    return a + b;
}

function main() {
    let total = add(2, 3);
}
```

The parts of the definition are:

1. `function` starts a function definition.
2. `add` is its name.
3. `a: i64` and `b: i64` are typed parameters.
4. `: i64` says that the function returns an `i64`.
5. `return a + b;` sends the result back to the caller.

The caller supplies arguments in the same order as the parameters. The number
and types of the arguments must match the definition.

## Functions that return nothing

A function without a return type returns `void`. Use `return;` to leave one
early, or let it reach the closing brace.

```vl
use std;

function announce(message: String) {
    std.print(message);
    std.print("\n");
}

function main() {
    announce("Starting");
}
```

Do not assign a `void` result to a variable. Call it as a statement instead.

## Returning a value

VL does not use the last expression in a function as an automatic return. A
function with a return type must use `return value;` on every path that reaches
the end of the function.

```vl
function larger(a: i64, b: i64): i64 {
    if (a > b) {
        return a;
    }
    return b;
}
```

`return;` is only for `void` functions. A returned value must have the type
declared after the parameter list.

## Calling functions from functions

Calls can be nested, and a function can call another function or itself.

```vl
function double(value: i64): i64 {
    return value + value;
}

function quadruple(value: i64): i64 {
    return double(double(value));
}

function main() {
    let result = quadruple(5);
}
```

Arguments are evaluated from left to right. User functions may be called from
any other function, including a function defined earlier or later in the file.

## Recursion

Recursion is when a function calls itself. A recursive function needs a base
case so that it eventually stops calling itself.

```vl
function countdown(n: u64) {
    if (n == 0) {
        return;
    }
    countdown(n - 1);
}
```

The `n == 0` branch is the base case. Without it, the function would keep
calling itself until the runtime could no longer continue.

## Type errors are caught early

VL checks calls before building the program. These calls are invalid because
one has the wrong number of arguments and the other has the wrong type:

```vl
function add(a: i64, b: i64): i64 {
    return a + b;
}

// add(1);          // missing one argument
// add("one", 2);   // first argument is not an i64
```

Run `check` while learning so the compiler can point at the call that needs
attention. Next, learn how arrays let you keep several values together.

Continue with [arrays and generics](/docs/arrays).

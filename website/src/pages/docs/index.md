---
layout: ../../layouts/Docs.astro
title: Documentation
description: A short, practical learning path for writing and running VL programs.
eyebrow: Learning path
availability: VL 0.1+
---

# Learn VL

Follow these pages in order. Each one introduces a small idea, explains why it
is useful, and gives you a complete example to change and run. You do not need
to understand compiler architecture to get started.

## 1. Get a program running

Start with [Getting started](/docs/getting-started) to set up the toolchain,
write `hello.vl`, check it, build it, and run the result on Naravm.

## 2. Learn the basics

Start with [Basics](/docs/basics) to learn the shape of a VL program, values,
types, variables, operators, and comments.

## 3. Make decisions and repeat work

[Conditions and loops](/docs/control-flow) explains `if`, `else`, `while`,
`break`, and `continue`. These are the tools that let a program react and do
the same work more than once.

## 4. Reuse your code

[Functions](/docs/functions) shows how to name work, accept inputs, return
results, and call one function from another.

## 5. Work with groups of values

[Arrays and generics](/docs/arrays) introduces fixed-length arrays and the
type parameters that let one function work with several element types.

## 6. Model named data

[Objects](/docs/objects) explains object declarations, named literals, field
access, and sharing.

## 7. Model either-or data

[Unions](/docs/unions) explains how to declare sum types, build variants,
and choose between them with `match`.

## 8. Work with text

[Strings](/docs/strings) covers string literals, escapes, and the standard
string helpers.

## 9. Organize a program

[Modules](/docs/modules) covers imports, source files, and the standard
library.

## 10. Use the command line

When you are ready to work with your own files, use the [CLI reference](/cli)
to check, build, inspect, and select a target from the `vl` driver.

## Where to go next

- Need module and function details? See the [standard library](/std).
- Building an integration? See the [compiler service](/internals/compiler-service).
- Contributing to the compiler? Read [Compiler internals](/internals).

VL is currently a small, evolving language. The examples in this guide match
the `0.1` compiler and the Naravm target.

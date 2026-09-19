---
layout: ../../layouts/Docs.astro
title: Documentation
description: A short, practical learning path for writing and running VL programs.
eyebrow: Learning path
availability: VL 0.1+
---

# Learn VL

Follow these pages in order to go from your first file to a working VL program.
You do not need to understand the compiler architecture to get started.

## 1. Get a program running

Start with [Getting started](/docs/getting-started) to set up the toolchain,
write `hello.vl`, check it, build it, and run the result on Naravm.

## 2. Learn the language

The [Language guide](/docs/language) covers values, expressions, functions,
conditionals, modules, strings, and the rules that affect everyday programs.

## 3. Use the command line

When you are ready to work with your own files, use the [CLI reference](/cli)
to check, build, inspect, and select a target from the `vl` driver.

## Where to go next

- Need module and function details? See the [standard library](/std).
- Building an integration? See the [compiler service](/internals/compiler-service).
- Contributing to the compiler? Read [Compiler internals](/internals).

VL is currently a small, evolving language. The examples in this guide match
the `0.1` compiler and the Naravm target.

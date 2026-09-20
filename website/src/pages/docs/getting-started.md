---
layout: ../../layouts/Docs.astro
title: Getting started
description: Write, check, build, and run your first VL program.
eyebrow: Article
availability: VL 0.1+
---

# Getting started

This guide takes you from a checkout to a running VL program. VL currently
builds for Naravm, and every executable starts at one required, zero-argument
entrypoint: `function main()`. (A missing return type means `void`, so this is
`function main(): void`.)

## Before you begin

You need Rust stable 1.80 or newer, Cargo, and a Naravm checkout. For now, VL
is run from the repository with Cargo rather than from a separately installed
binary.

## Write a first program

Create `hello.vl` with one import and one entrypoint:

```vl
use std.println;

function main() {
    println("Hello, world!");
}
```

The `println` call writes the string and ends the line automatically. A plain
`print` also exists for output without an added newline — `println("Hello,
world!")` prints the same bytes as `print("Hello, world!\n")`.

## Check the source

From the VL repository root, ask the compiler to validate the file:

```sh
cargo run -- check examples/hello.vl
```

A clean program exits with status 0. If the source is invalid, the command
returns status 1 and prints an Ariadne diagnostic pointing at the relevant
span.

## Build and run it

Build a Naravm vmfile with `--out`:

```sh
cargo run -- build examples/hello.vl --out /tmp/hello.nara
```

Run it from the Naravm checkout:

```sh
zig build run -- /tmp/hello.nara
# Hello, world!
```

## If something goes wrong

Try `check` before `build` when you are editing a file. The diagnostic shows
the source span and the first problem the compiler found. Fix that problem and
run the command again; VL keeps later stages quiet when an earlier stage has
already reported the cause.

## Continue learning

- Learn the syntax and values in [Basics](/docs/basics), then continue through
  [conditions and loops](/docs/control-flow) and [functions](/docs/functions).
- Learn about [arrays and generics](/docs/arrays) and [modules and strings](/docs/modules).
- See every command and inspection option in the [CLI reference](/cli).
- Look up modules and functions in the [standard library](/std).

---
layout: ../../layouts/Docs.astro
title: Getting started
description: Check, build, and run a VL program on Naravm.
eyebrow: Article
availability: VL 0.1+
---

# Getting started

The compiler currently targets Naravm. A VL executable has one required,
zero-argument entrypoint: `function main()`.

```text
vl check <file>
```

## Overview

You need Rust stable 1.80 or newer, Cargo, and a checkout of Naravm. Clone
both repositories; the compiler lives at the workspace root.

Check a program:

```sh
cargo run -- check examples/hello.vl
```

A clean program exits 0. A broken one exits 1 with an Ariadne report pointing
at the span.

The hello-world example imports the standard module and calls `std.print`:

```vl
use std;

function main() {
    std.print("Hello, world!\n");
}
```

Build a Naravm vmfile:

```sh
cargo run -- build examples/hello.vl --out /tmp/hello.nara
```

Run it from the Naravm checkout:

```sh
zig build run -- /tmp/hello.nara
# Hello, world!
```

## Topics

### Editor setup and file association

Stub. How to associate `.vl` files and where diagnostics surface.

### A first program, walked line by line

`hello.vl` demonstrates the required `main`, dotted VL namespaces, and the
Naravm `std::print` native function. VL writes `std.print`; the backend maps
that name to Naravm's double-colon namespace without changing the VM.

### How to read an Ariadne diagnostic

Stub. Spans, labels, and why one root cause stays one error.

---
layout: ../../layouts/Docs.astro
title: Getting started
description: Install Rust, check a program, build it, look inside.
eyebrow: Article
availability: VL 0.1+
---

# Getting started

A sketch of the final guide. The commands below already work.

```text
vl check <file>
```

## Overview

You need Rust stable 1.80 or newer, plus Cargo. Nothing else. Clone the repository; the compiler lives at the workspace root.

Check a program:

```sh
cargo run -- check examples/hello.vl
```

A clean program exits 0 and stays silent. A broken one exits 1 with an Ariadne report pointing at the span.

Build it and look inside:

```sh
cargo run -- build examples/arith.vl --emit lir
cargo run -- build examples/arith.vl --target stackvm
```

## Topics

### Editor setup and file association

Stub. How to associate `.vl` files and where diagnostics surface.

### A first program, walked line by line

Stub. `hello.vl` from the first `function` to the exit code.

### How to read an Ariadne diagnostic

Stub. Spans, labels, and why one root cause stays one error.

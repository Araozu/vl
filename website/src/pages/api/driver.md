---
layout: ../../layouts/Docs.astro
title: The driver
description: CLI wiring and the only place that prints a diagnostic.
eyebrow: Reference
availability: VL 0.1+
section: api
---

# The driver

The source is `src/main.rs` at the workspace root. It reads files, drives the stages in order, renders every diagnostic with `emit_all`, and chooses the exit code: 0 when clean, 1 when anything was reported.

## Overview

The driver owns the CLI, the file I/O, the exit codes, and all printing. Libraries return diagnostics; they never print.

## Topics

### Parsing the CLI with clap

Stub. `check`, `build`, `lex`, `parse`, and `targets`.

### Reading `.vl` sources off disk

Stub. Sources table, spans, and how paths reach diagnostics.

### Rendering through Ariadne, in colour, on stderr

Stub. Worked sessions beside the [CLI reference](/docs/cli).

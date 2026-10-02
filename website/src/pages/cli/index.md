---
layout: ../../layouts/Docs.astro
title: CLI reference
description: Check, build, format, and run VL programs from the command line.
eyebrow: Reference
availability: VL 0.1+
---

# CLI reference

The `vl` driver reads `.vl` files, runs the compiler, and reports diagnostics.
From a repository checkout, run it with `cargo run --`; once installed, use
`vl` directly.

## Commands

### `init [module]`

Create a `vl.toml` project file and a `src/` directory in the current folder.
The optional module name defaults to the current folder name:

```sh
vl init my_app
```

### `check <file>`

Validate a source file without producing a target artifact. The command exits
0 when the file is valid and 1 when it reports source diagnostics.

```sh
vl check examples/hello.vl
```

Pass `-` to read standard input as module `stdin`; this skips project lookup.
`--format json` writes one machine-readable report to stdout. Each diagnostic
includes its file, severity, message, optional code and note, and labelled
spans with byte offsets and 1-based line and column locations:

```sh
vl check examples/hello.vl --format json
echo 'val x = 1' | vl check - --format json
```

### `build [file]`

Compile one file for the default `naravm` target:

```sh
vl build examples/hello.vl --out /tmp/hello.nara
```

Use `--out <path>` to write output to a file. Without it, text output goes to
stdout; binary target output also goes to stdout, so use a file for Naravm
artifacts. Omit the file to build the project in the current directory from
`vl.toml`.

`--format json` writes diagnostics as JSON. For `build`, also provide `--out`
so compiler output does not get mixed into the JSON stream:

```sh
vl build examples/hello.vl --format json --out /tmp/hello.nara
cat examples/hello.vl | vl build - --emit lir
```

### `fmt <path>...`

Format `.vl` files in place. Directories are searched recursively. The VL
formatter uses a fixed Zig-inspired style; `--check` reports whether a file
would change without writing it. Files with lexing or parsing errors are left
unchanged and reported:

```sh
vl fmt examples/hello.vl
vl fmt --check examples/hello.vl
```

### `run [name]`

Run a project script from the `[scripts]` table in `vl.toml`. With no name,
`vl run` executes the `run` script. Commands run from the project root through
the platform shell:

```toml
[scripts]
run = "vl build"
check = "vl check src/main.vl"
```

```sh
vl run
vl run check
```

### `lsp`

Start the Language Server Protocol server over stdio for editor clients:

```sh
vl lsp
```

It provides diagnostics, hover, go to definition, document symbols, formatting,
and completion. See [Editor support](/docs/editor-support) for VS Code and
Neovim setup.

### `lex <file>` and `parse <file>`

Print the tokens or syntax tree produced from a source file. These commands are
useful when investigating comments, literals, or punctuation:

```sh
vl lex examples/hello.vl
vl parse examples/hello.vl
```

### `targets`

List the code-generation targets known to the compiler:

```sh
vl targets
```

### `stdlib`

Export the current standard library API as JSON, combining target-native
functions and embedded VL helpers. The output includes module names, parameter
and return types, generic bounds, and error variants. Website builds use this
catalog to generate the standard library reference:

```sh
vl stdlib
```

`naravm` produces a runnable Naravm vmfile and is the only supported target.

## Build options

| Option | Purpose |
| --- | --- |
| `--target <name>` | Select a target such as `naravm`. |
| `--emit <kind>` | Dump `tokens`, `ast`, `lir`, or `asm` instead of a final artifact. |
| `--out <path>` | Write output to a file instead of stdout. Required with `--format json`. |
| `--format` | Render diagnostics as `human` text (stderr) or `json` (stdout). |

Examples:

```sh
vl build examples/arith.vl --emit lir
vl build examples/arith.vl --target naravm
vl build examples/hello.vl --target naravm --out /tmp/hello.nara
```

For the language itself, continue with the [learning path](/docs). For module
and function details, see the [standard library](/std).

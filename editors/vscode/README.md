# VL Language Support

VS Code syntax highlighting, editor configuration, and language server
client for the VL programming language.

## Features

- Syntax highlighting for `.vl` source files
- Bracket and quote auto-closing
- Comment toggling with `//`
- Brace-based indentation
- Language server (`vl lsp` over stdio): diagnostics, hover,
  goto-definition, document symbols, formatting, completion

## Language server

The extension spawns `vl lsp` from `PATH`. Build the repo with
`cargo build` and make sure `vl` is installed or on `PATH`. To use a
different binary (e.g. a local debug build), set:

```json
{
  "vl.server.path": "/home/you/projects/rust/vl/target/debug/vl"
}
```

## Build

From this directory:

```sh
pnpm install --frozen-lockfile
pnpm package
```

This produces `vl-language-support.vsix`, which can be installed with:

```sh
code --install-extension vl-language-support.vsix
```

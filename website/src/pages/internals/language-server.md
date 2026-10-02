---
layout: ../../layouts/Docs.astro
title: Language server
description: The VL stdio language server and its editor-facing features.
eyebrow: Internals
availability: VL 0.1+
---

# Language server

The `vl-lsp` crate implements the Language Server Protocol over stdio. The
driver starts it with `vl lsp`; editor clients live in `editors/vscode` and
`editors/neovim`.

## Shared analysis

Each open buffer is checked with `vl-frontend::check_text`, the in-memory
frontend used by other tooling. The server builds the same default module
catalog as the driver by merging Naravm native signatures with the embedded
standard library. Lexing, parsing, resolution, type checking, and the world
plan therefore share compiler diagnostics with `vl check`.

The language server keeps a best-effort name resolution for parsed buffers,
even when type checking reports errors. That lets hover and go to definition
continue to work while a file is being edited. Syntax trees also power
document symbols; `vl-fmt` provides formatting.

## Protocol support

The server syncs full document text and publishes diagnostics after open,
change, and save notifications. It supports hover, go to definition, document
symbols, full-document formatting, and completion for VL keywords and names
visible in the current file. The stdio transport owns process I/O; analysis
helpers take source text and a pre-merged catalog.

For editor setup, see [Editor support](/docs/editor-support). For the CLI
command, see [`vl lsp`](/cli#vl-lsp).

---
layout: ../../layouts/Docs.astro
title: Editor support
description: Set up VL syntax highlighting and language server features in VS Code or Neovim.
eyebrow: Tools
availability: VL 0.1+
---

# Editor support

The repository includes syntax support for VS Code and Neovim. Both can also
connect to the `vl lsp` language server for diagnostics, hover information,
go to definition, document symbols, formatting, and completion.

## VS Code

Build and install the extension from the repository:

```sh
cd editors/vscode
pnpm install --frozen-lockfile
pnpm package
code --install-extension vl-language-support.vsix
```

The extension starts `vl lsp` from your `PATH`. Build the VL compiler with
`cargo build` and make sure its `vl` binary is available. To choose a specific
binary, set `vl.server.path` in VS Code settings:

```json
{
  "vl.server.path": "/path/to/vl"
}
```

The extension includes syntax highlighting, bracket and quote pairing,
`//` comment toggling, and brace indentation.

## Neovim

The repository provides native syntax highlighting and indentation, with an
optional Tree-sitter parser. It also starts `vl lsp` for `.vl` buffers on
Neovim versions with the built-in LSP client. Put `vl` on your `PATH`, or set
`vim.g.vl_lsp_cmd` to the compiler path and command:

```lua
vim.g.vl_lsp_cmd = { "/path/to/vl", "lsp" }
```

For setup with `lazy.nvim`, Tree-sitter, and suggested keymaps, follow the
[Neovim setup guide](https://github.com/Araozu/vl/blob/develop/editors/neovim/README.md).
For extension packaging and detailed VS Code instructions, see the
[VS Code setup guide](https://github.com/Araozu/vl/blob/develop/editors/vscode/README.md).

## Language server features

`vl lsp` uses the same parser and type checker as `vl check`. It publishes
diagnostics as a buffer changes and provides:

- Hover signatures and symbol information
- Go to definition within the open document
- Document symbols
- Keyword and in-scope name completion
- Document formatting using the `vl fmt` style

The compiler formatter rejects malformed source instead of changing it; fix
syntax errors before requesting formatting. See the [CLI reference](/cli) for
standalone checking and formatting commands.

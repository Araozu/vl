# VL for Neovim

Native Neovim runtime support for VL files:

- `.vl` filetype detection
- Syntax highlighting
- `//` comment settings
- Brace-aware indentation
- Optional Tree-sitter highlighting, indentation, folds, and incremental selection

The native Vim syntax and indentation files are used as a fallback when no
VL Tree-sitter parser is installed.

## Language server (`vl lsp`)

`plugin/vl-lsp.lua` starts the VL language server over stdio for every `.vl`
buffer via Neovim's built-in LSP client (diagnostics, hover,
goto-definition, symbols, formatting, completion). It needs `vl` on `PATH`
(build the repo with `cargo build` and link or copy `target/debug/vl`).

To use a different server binary (e.g. a local debug build):

```lua
vim.g.vl_lsp_cmd = { vim.fn.expand("~/projects/rust/vl/target/debug/vl"), "lsp" }
```

The workspace root is the nearest `vl.toml`, else the nearest `.git`, else
the file's directory. Suggested keymaps (attach to `LspAttach` as usual):

```lua
vim.api.nvim_create_autocmd("LspAttach", {
  callback = function(args)
    local client = vim.lsp.get_client_by_id(args.data.client_id)
    if client and client.name == "vl" then
      local opts = { buffer = args.buf }
      vim.keymap.set("n", "gd", vim.lsp.buf.definition, opts)
      vim.keymap.set("n", "K", vim.lsp.buf.hover, opts)
      vim.keymap.set("n", "<leader>f", vim.lsp.buf.format, opts)
    end
  end,
})
```

## lazy.nvim

Because the runtime files live in a subdirectory of the VL repository, add that
directory to `runtimepath` during Lazy initialization:

```lua
{
  "Araozu/vl",
  name = "vl",
  lazy = false,
  init = function(plugin)
    vim.opt.runtimepath:append(plugin.dir .. "/editors/neovim")
    vim.filetype.add({ extension = { vl = "vl" } })
  end,
}
```

For Tree-sitter, add the runtime path to the VL plugin as above and install
`nvim-treesitter`:

```lua
{
  "nvim-treesitter/nvim-treesitter",
  build = ":TSUpdate",
  opts = {
    highlight = { enable = true },
    indent = { enable = true },
    ensure_installed = { "vl" },
    incremental_selection = {
      enable = true,
      keymaps = {
        init_selection = "gnn",
        node_incremental = "grn",
        scope_incremental = "grc",
        node_decremental = "grm",
      },
    },
  },
}
```

The VL plugin automatically registers its local generated parser with
`nvim-treesitter`. Run `:TSInstall vl` after opening Neovim. The parser is
generated for Tree-sitter ABI 15 and is built locally for the current platform;
the repository intentionally does not commit a platform-specific `.so` file.
The runtime uses the queries in `queries/vl` for highlighting, indentation, and
folds. Without `nvim-treesitter`, native Vim syntax and brace indentation still
work.

The grammar source and reproducible generated parser sources live in
`editors/tree-sitter-vl`. From that directory:

```sh
tree-sitter generate
tree-sitter test
```

Run `:Lazy sync`, then open a `.vl` file. Verify detection with:

```vim
:set filetype?
```

The result should be `filetype=vl`.

### Local development

To load a local checkout instead of the repository plugin, use `dir` in the
Lazy spec:

```lua
{
  dir = vim.fn.expand("~/projects/rust/vl"),
  name = "vl",
  lazy = false,
  init = function(plugin)
    vim.opt.runtimepath:append(plugin.dir .. "/editors/neovim")
    vim.filetype.add({ extension = { vl = "vl" } })
  end,
}
```

Change the path to the location of your local VL checkout.

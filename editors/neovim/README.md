# VL for Neovim

Native Neovim runtime support for VL files:

- `.vl` filetype detection
- Syntax highlighting
- `//` comment settings
- Brace-aware indentation
- Optional Tree-sitter highlighting, indentation, folds, and incremental selection

No language server is required. The native Vim syntax and indentation files are
used as a fallback when no VL Tree-sitter parser is installed.

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

For Tree-sitter, add the runtime path to the VL plugin as above and configure
`nvim-treesitter` separately:

```lua
{
  "nvim-treesitter/nvim-treesitter",
  build = ":TSUpdate",
  opts = {
    highlight = { enable = true },
    indent = { enable = true },
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

The parser must be installed separately and registered under the `vl` language
name; this repository does not ship a generated parser yet. When a parser is
available, the runtime automatically uses the queries in `queries/vl` for
highlighting, indentation, and folds. Without one, the native Vim syntax and
brace indentation continue to work.

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

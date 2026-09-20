# VL for Neovim

Native Neovim runtime support for VL files:

- `.vl` filetype detection
- Syntax highlighting
- `//` comment settings
- Brace-aware indentation

No language server or Tree-sitter parser is required.

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

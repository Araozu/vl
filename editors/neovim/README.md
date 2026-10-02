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

### Testing the server locally

Build the binary from the repo root, then point the plugin at it so you
can iterate on the server without installing anything:

```sh
cargo build   # produces ./target/debug/vl
```

```lua
-- before opening any .vl file (e.g. in your lazy spec's `init`):
vim.g.vl_lsp_cmd = { vim.fn.expand("~/projects/rust/vl/target/debug/vl"), "lsp" }
```

Combined with a local plugin checkout, a full local wiring looks like:

```lua
{
  dir = vim.fn.expand("~/projects/rust/vl"),
  name = "vl",
  lazy = false,
  init = function(plugin)
    vim.opt.runtimepath:append(plugin.dir .. "/editors/neovim")
    vim.filetype.add({ extension = { vl = "vl" } })
    vim.g.vl_lsp_cmd = { plugin.dir .. "/target/debug/vl", "lsp" }
  end,
}
```

Then verify, with a `.vl` file open:

1. `:LspInfo` shows a `vl` client attached with the expected root dir
   (nearest `vl.toml`, else nearest `.git`, else the file's directory).
2. Break something (e.g. delete a `;`) — a diagnostic should appear;
   `K` on a call shows its signature, `gd` jumps to the definition.
3. After rebuilding the binary (`cargo build`), restart the server with
   `:LspRestart vl` (Neovim 0.10+) instead of restarting the editor.
4. If the client never attaches, check `:LspLog` and confirm
   `target/debug/vl lsp` runs (it speaks LSP over stdio and stays silent
   until a client talks to it).

The server's own test suite (`cargo test -p vl-lsp`) covers positions,
hover/goto, completion, formatting, and the stdio protocol handling.

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

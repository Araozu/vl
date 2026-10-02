-- VL language server (`vl lsp`) for Neovim's built-in LSP client.
--
-- Starts one `vl lsp` server per VL workspace (nearest `vl.toml`, else the
-- nearest `.git`, else the file's directory). Override the server command
-- with `vim.g.vl_lsp_cmd`, e.g. `vim.g.vl_lsp_cmd = { "vl", "lsp" }` is the
-- default; a local build can point at `~/projects/rust/vl/target/debug/vl`.
-- Requires Neovim with `vim.lsp.start` (0.8+); older versions keep syntax
-- and indentation support and skip this file silently.

if not (vim.lsp and vim.lsp.start) then
  return
end

local function root_dir(fname)
  if vim.fs and vim.fs.root then
    return vim.fs.root(fname, { "vl.toml", ".git" })
  end
  return vim.fn.fnamemodify(fname, ":p:h")
end

vim.api.nvim_create_autocmd("FileType", {
  pattern = "vl",
  group = vim.api.nvim_create_augroup("vl-lsp", { clear = true }),
  callback = function(args)
    local cmd = vim.g.vl_lsp_cmd or { "vl", "lsp" }
    if vim.fn.executable(cmd[1]) == 0 then
      vim.notify(
        string.format("[vl] language server not found: %s (set vim.g.vl_lsp_cmd)", cmd[1]),
        vim.log.levels.WARN
      )
      return
    end
    vim.lsp.start({
      name = "vl",
      cmd = cmd,
      root_dir = root_dir(args.file),
    })
  end,
})

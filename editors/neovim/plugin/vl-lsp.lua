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
  local dir = vim.fn.fnamemodify(fname, ":p:h")
  if vim.fs and vim.fs.root then
    -- `vim.fs.root` returns nil when no marker is found; fall back to
    -- the file's directory instead of handing `vim.lsp.start` a nil root.
    return vim.fs.root(fname, { "vl.toml", ".git" }) or dir
  end
  -- Pre-0.10 Neovim without `vim.fs`: search upward manually.
  local vl_toml = vim.fn.findfile("vl.toml", dir .. ";")
  if vl_toml ~= "" then
    return vim.fn.fnamemodify(vl_toml, ":p:h")
  end
  local git = vim.fn.finddir(".git", dir .. ";")
  if git ~= "" then
    return vim.fn.fnamemodify(git, ":p:h")
  end
  return dir
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

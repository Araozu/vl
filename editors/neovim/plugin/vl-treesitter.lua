local runtime_file = vim.api.nvim_get_runtime_file("plugin/vl-treesitter.lua", false)[1]
if not runtime_file then
  return
end

local grammar_path = vim.fs.normalize(vim.fs.joinpath(
  vim.fs.dirname(runtime_file),
  "..",
  "..",
  "tree-sitter-vl"
))

local function register_vl()
  local ok, parsers = pcall(require, "nvim-treesitter.parsers")
  if not ok then
    return
  end

  parsers.vl = {
    install_info = {
      path = grammar_path,
      generate = false,
      queries = "../neovim/queries/vl",
    },
    tier = 4,
  }
end

vim.api.nvim_create_autocmd("User", {
  pattern = "TSUpdate",
  callback = register_vl,
})
register_vl()

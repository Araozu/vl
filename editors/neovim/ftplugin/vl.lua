vim.bo.commentstring = "// %s"
vim.bo.comments = "://"
vim.bo.suffixesadd = ".vl"

-- Use Tree-sitter when a VL parser is installed, but keep the native runtime
-- files usable on a stock Neovim installation.
local has_parser = vim.treesitter and vim.treesitter.get_parser
local parser
if has_parser then
  has_parser, parser = pcall(vim.treesitter.get_parser, 0, "vl")
end
if has_parser and parser and vim.treesitter.start then
  vim.treesitter.start(0, "vl")
  vim.bo.syntax = "off"

  if vim.treesitter.indentexpr then
    vim.bo.cindent = false
    vim.bo.indentexpr = "v:lua.vim.treesitter.indentexpr()"
  end

  if vim.treesitter.foldexpr then
    vim.wo.foldmethod = "expr"
    vim.wo.foldexpr = "v:lua.vim.treesitter.foldexpr()"
    vim.wo.foldlevel = 99
  end
end

vim.b.undo_ftplugin = table.concat({
  "setlocal commentstring< comments< suffixesadd< syntax< cindent< indentexpr<",
  "setlocal foldmethod< foldexpr< foldlevel<",
}, " ")

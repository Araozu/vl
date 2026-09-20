# Tree-sitter VL

This directory contains the Tree-sitter grammar for VL and its generated parser
sources.

```sh
tree-sitter generate
tree-sitter test
tree-sitter build --output parser.so
```

`src/parser.c`, `src/grammar.json`, and `src/node-types.json` are generated and
tracked so Neovim can compile the parser without requiring JavaScript grammar
generation. `parser.so` is platform-specific and must not be committed.

The grammar targets Tree-sitter ABI 15, which is the ABI used by current
Neovim releases. The Neovim runtime integration registers this directory as a
local `nvim-treesitter` parser and supplies the queries from
`editors/neovim/queries/vl`.

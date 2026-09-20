# VL Language Support

VS Code syntax highlighting and editor configuration for the VL programming
language.

## Features

- Syntax highlighting for `.vl` source files
- Bracket and quote auto-closing
- Comment toggling with `//`
- Brace-based indentation

## Build

From this directory:

```sh
pnpm install --frozen-lockfile
pnpm package
```

This produces `vl-language-support.vsix`, which can be installed with:

```sh
code --install-extension vl-language-support.vsix
```

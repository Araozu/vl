# VL v0 — Syntax Grammar (`vl-syntax`)

Source: `crates/vl-syntax/src/lib.rs`. Recursive descent, tokens → AST.
Signature: `parse(toks: &[Token], _src: &str) -> (Program, Vec<Diagnostic>)`.
Input tokens come from `vl-lex` (`crates/vl-lex/GRAMMAR.md`); output is `Program`.
Parser recovers per-item (and per-stmt inside `function`); one bad item hides no others.

## Grammar (as implemented)

```text
program := item*
item    := use_item | let_item | function_item
use_item := "use" path ("." "{" ident ("," ident)* "}")? ";"
let_item := "let" ident "=" expr ";"
function_item := "function" ident "(" params? ")" block
params   := ident ("," ident)*          ; no trailing comma
block    := "{" stmt* "}"
stmt     := let_stmt | expr_stmt
let_stmt := "let" ident "=" expr ";"
expr_stmt := expr ";"                   ; mandatory, TS-style
expr     := term (("+" | "-") term)*     ; left-assoc
term     := factor (("*" | "/") factor)* ; left-assoc
factor   := call | int | string | path | "(" expr ")" | "-" factor
path     := ident ("." ident)*
call     := path "(" args? ")"
args     := expr ("," expr)*
```

Terminal names are `vl-lex` `TokenKind`s: `Let Function Eq Semi LParen RParen
LBrace RBrace Comma Dot Plus Minus Star Slash Ident Int String Eof`.

### Notes

* Semicolons are mandatory everywhere: `let` needs `;`, expression-statements
  need `;` — including the last statement of a function body
  (`{ let d = x; d; }`). A bare trailing `d` without `;` is `E100`.
* Unary is `-` only, right-recursive: `- -5`, `--x` ok; `+x`, `!x` → `E103`.
* Parens are transparent in the AST: `(e)` returns inner `Expr`, span drops parens.
* Calls are identifier calls only: `foo(...)`; member calls, function-valued
  calls, indexing, `.`, `return`, `if`, `else`, and `while` do not exist.

## AST

```text
Program { items: Vec<Item> }
Item ::= Let { name, name_span, value: Expr, span }
       | Function { name, name_span, params: Vec<(String, Span)>, body: Vec<Stmt>, span }
Stmt ::= Let { name, name_span, value: Expr, span }
       | Expr(Expr)
Expr ::= Int(i64, Span) | String(Vec<u8>, Span) | Var { path, span }
         | Call { callee: path, callee_span, args, span }
        | Unary { op: Neg, rhs, span } | Binary { op, lhs, rhs, span }
BinOp ::= Add | Sub | Mul | Div
UnOp  ::= Neg
```

Spans (`vl_common::Span`, byte, half-open): `let` spans `let..;`, `function` spans
`function..}`, `Binary` spans `lhs.start..rhs.end`, `Unary` spans `minus.start..rhs.end`.

## Errors (all `Severity::Error`)

| Code | When | Message shape |
|---|---|---|
| `E100` | `expect()` mismatch (missing `= ; ( ) { }`) | `expected {what}, found {describe}` + label `unexpected token here` |
| `E101` | item doesn't start with `let`/`function` | `expected an item (\`let\` or \`function\`), found …` + label `items start with …` |
| `E102` | missing name (after `let`/`function`, or bad param) | `expected a name, found …` + label `expected identifier here` |
| `E103` | bad expression start | `expected an expression, found …` + label `expected value here` |

`describe()`: `Ident(n)` → `` identifier `n` ``, `Int(v)` → `` integer `v` ``,
keywords/symbols backticked, `Eof` → `end of file`.
Missing `;` (`let x = 1`) → `E100`; `@` never reaches here (lexer `E000`).

## Recovery

* `program` loop: failed `parse_item()` → `recover_to_item_boundary`: skip
  until (and consuming) `;`/`}`, or stopping at `let`/`function`/`Eof`.
* `function` body loop: failed `parse_stmt()` → `recover_to_stmt_boundary`: skip
  until (and consuming) `;`, or stopping at `}`/`let`/`function`/`Eof`.
* `parse_expr/term` return `None` upward on missing rhs, so `1 +` abandons
  the whole item/stmt and recovers at the boundary. Poison rule (AGENTS.md):
  failed nodes are dropped, no cascading diag downstream.

## Examples

```text
"let x = 1 + 2 * 3;"                → Item::Let, Binary(Add, 1, Binary(Mul, 2, 3))
"function main() { let d = x; d; }" → Item::Function { params: [], body: [Let(d), Expr(Var d)] }
"function f(a, b) { a; }"           → params [a, b]
"let x = 1"                         → E100 (expected `;`), item dropped
"function main() { d }"             → E100 (expected `;`), stmt dropped
"let x = - -5;"                     → Unary(Neg, Unary(Neg, 5))
"d;" at top level                   → E101 (items start with let/function)
```

## Explicitly NOT syntax in v0

No `return`, no `if`/`else`/`while`, no types/annotations
(TS-like surface only: `let`, `function`, calls, braces, mandatory `;`),
no trailing comma in params, no bool literals.

## Modules

Each source file is a module named after its filename without the `.vl`
extension. `use std.string;` brings the `string` module name into scope, but
not its exports, so members are written `string.new()`. Grouped imports bring
only listed exports into scope: `use std.fs.{open, read};`.

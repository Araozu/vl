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
let_item := "let" ident (":" type)? "=" expr ";"
function_item := "function" ident "(" params? ")" (":" type)? block
params   := param ("," param)*          ; no trailing comma
param    := ident ":" type
type     := "u64" | "i64" | "f64" | "bool" | "u8" | "string" | "File" | "void"
block    := "{" stmt* "}"
stmt     := let_stmt | assign_stmt | if_stmt | while_stmt | break_stmt | continue_stmt | return_stmt | expr_stmt
let_stmt := "let" ident (":" type)? "=" expr ";"
assign_stmt := ident "=" expr ";"
if_stmt  := "if" "(" expr ")" branch ("else" branch)?
while_stmt := "while" "(" expr ")" branch
break_stmt := "break" ";"
continue_stmt := "continue" ";"
return_stmt := "return" expr? ";"
branch   := block | stmt
expr_stmt := expr ";"                   ; mandatory, TS-style; value discarded (no implicit return)
expr     := or
or       := and ("||" and)*
and      := equality ("&&" equality)*
equality := comparison (("==" | "!=") comparison)*
comparison := term (("<" | "<=" | ">" | ">=") term)*
term     := factor (("+" | "-") factor)* ; left-assoc
factor   := unary (("*" | "/") unary)*   ; left-assoc
unary    := ("-" | "!") unary | call
call     := path "(" args? ")"
args     := expr ("," expr)*
literal  := int | i64 | u64 | f64 | u8 | bool
path     := ident ("." ident)*
```

Terminal names are `vl-lex` `TokenKind`s: `Let Function If Else While Break
Continue Return Eq Semi LParen RParen LBrace RBrace Comma Dot Colon Plus Minus
Star Slash EqEq Bang BangEq Lt LtEq Gt GtEq AmpAmp PipePipe Ident I64 U64 F64 U8
Bool String Eof`.

### Notes

* Semicolons are mandatory everywhere: `let`, `return`, `break`, `continue`,
  and expression-statements need `;` — including the last statement of a
  function body (`{ let d = x; }`). A bare trailing `d` without `;` is `E100`.
* There are no implicit returns: only `return expr;` yields a value
  (`return;` for `void`). A trailing `d;` is a discarded expression statement.
* Unary is `-` / `!`, right-recursive: `- -5`, `!x` ok; `+x` → `E103`.
* Parens are transparent in the AST: `(e)` returns inner `Expr`, span drops parens.
* Calls are callee-by-name (`ident(args)`), TypeScript-style, so forward
  references to `function` items work.

## AST

```text
Program { items: Vec<Item> }
Item ::= Use { path, names, span }
       | Let { name, name_span, value: Expr, span }
       | Function { name, name_span, params: Vec<Param>, ret: Option<VlType>, ret_span, body: Vec<Stmt>, span }
Param ::= { name, name_span, ty: Option<VlType>, ty_span }
Stmt ::= Let { name, name_span, value: Expr, span }
       | Assign { name, name_span, value: Expr, span }
       | If { condition, then_body, else_body, span }
       | While { condition, body, span }
       | Break { span } | Continue { span }
       | Return { value: Option<Expr>, span }
       | Expr(Expr)
Expr ::= Literal(Scalar, Span) | String(Vec<u8>, Span) | Var { path, span }
         | Call { callee: path, callee_span, args, span }
         | Unary { op, rhs, span } | Binary { op, lhs, rhs, span }
BinOp ::= Add | Sub | Mul | Div | Eq | Ne | Lt | Le | Gt | Ge | And | Or
UnOp  ::= Neg | Not
```

Spans (`vl_common::Span`, byte, half-open): `let` spans `let..;`, `function` spans
`function..}`, `Binary` spans `lhs.start..rhs.end`, `Unary` spans `minus.start..rhs.end`.

## Errors (all `Severity::Error`)

| Code | When | Message shape |
|---|---|---|
| `E100` | `expect()` mismatch (missing `= ; ( ) { }`) | `expected {what}, found {describe}` + label `unexpected token here` |
| `E101` | item doesn't start with `use`/`let`/`function` | `expected an item (\`use\`, \`let\` or \`function\`), found …` + label `items start with …` |
| `E102` | missing name (after `let`/`function`, or bad param) | `expected a name, found …` + label `expected identifier here` |
| `E103` | bad expression start | `expected an expression, found …` + label `expected value here` |
| `E104` | missing param type / `void` param | `parameter \`{name}\` is missing a type` / `cannot be \`void\`` |
| `E105` | unknown type | `unknown type …` |

`describe()`: `Ident(n)` → `` identifier `n` ``, `Int(v)` → `` integer `v` ``,
keywords/symbols backticked, `Eof` → `end of file`.
Missing `;` (`let x = 1`) → `E100`; `@` never reaches here (lexer `E000`).

## Recovery

* `program` loop: failed `parse_item()` → `recover_to_item_boundary`: skip
  until (and consuming) `;`/`}`, or stopping at `let`/`function`/`Eof`.
* `function` body loop: failed `parse_stmt()` → `recover_to_stmt_boundary`: skip
  until (and consuming) `;`, or stopping at `}`/`let`/`function`/`if`/`while`/
  `break`/`continue`/`return`/`Eof`.
* `parse_expr/term` return `None` upward on missing rhs, so `1 +` abandons
  the whole item/stmt and recovers at the boundary. Poison rule (AGENTS.md):
  failed nodes are dropped, no cascading diag downstream.

## Examples

```text
"let x = 1 + 2 * 3;"                → Item::Let, Binary(Add, 1, Binary(Mul, 2, 3))
"function main() { let d = x; }"    → Item::Function { params: [], body: [Let(d)] }
"function add(a: i64, b: i64): i64 { return a + b; }" → Item::Function { body: [Return(Binary(Add))] }
"function main() { return; }"       → Item::Function { body: [Return(None)] }
"let x = 1"                         → E100 (expected `;`), item dropped
"function main() { d }"             → E100 (expected `;`), stmt dropped
"function f(): i64 { return 1 }"    → E100 (expected `;`), stmt dropped
"let x = - -5;"                     → Unary(Neg, Unary(Neg, 5))
"d;" at top level                   → E101 (items start with use/let/function)
```

## Modules

Each source file is a module named after its filename without the `.vl`
extension. `use std.string;` brings the `string` module name into scope, but
not its exports, so members are written `string.len()`. Grouped imports bring
only listed exports into scope: `use std.fs.{open, read};`. A trailing export
can be imported directly: `use std.print;` behaves like `use std.{print};` and
brings `print` into scope.

# VL v0 — Syntax Grammar (`vl-syntax`)

Source: `crates/vl-syntax/src/lib.rs`. Recursive descent, tokens → AST.
Signature: `parse(toks: &[Token], _src: &str) -> (Program, Vec<Diagnostic>)`.
Input tokens come from `vl-lex` (`crates/vl-lex/GRAMMAR.md`); output is `Program`.
Parser recovers per-item (and per-stmt inside `fun`); one bad item hides no others.

## Grammar (as implemented)

```text
program := item*
item    := use_item | binding_item | function_item | object_item | union_item
use_item := "use" path ("." "{" ident ("," ident)* "}")? ";"
binding_item := ("var" | "val") (ident | destructure) (":" type)? "=" expr ";"
destructure := "#" "(" destructure_binding ("," destructure_binding)* ","? ")"
destructure_binding := ident (":" ident)?
function_item := "fun" ident type_params? "(" params? ")" (":" type)? block
type_params := "[" type_param ("," type_param)* "]"
type_param := ident ("extends" ("Numeric" | "Comparable"))?
object_item := "type" ident "=" "object" "{" object_member* "}" ";"
union_item := "type" ident type_params? "=" "union" "{" union_variants? "}" ";"
union_variants := union_variant ("," union_variant)* ","?
union_variant := ident ("(" type ("," type)* ")")?
object_member := object_field | assoc_fn
object_field := ident ":" type ","?    ; the comma may be omitted before `fun` or `}`
assoc_fn := "fun" ident type_params? "(" params? ")" (":" type)? block ","?
                                       ; a trailing comma after a `fun` member is allowed, never required
params   := param ("," param)*          ; no trailing comma
param    := ident ":" type
type     := nullable_type | mutable_type | type_atom
nullable_type := "?" type               ; nullable sugar (`?u64` desugars to the builtin `Option` union)
mutable_type := "*" (type_atom | nullable_type)   ; one capability qualifier (`*Foo`, `*Array[T]`, `*?Foo`)
type_atom := "u64" | "i64" | "f64" | "bool" | "u8" | "String" | "File" | ident | union_type | "Array" "[" type "]" | tuple_type | "void"
union_type := ident ("[" type ("," type)* "]")?   ; bare `Option` or applied `Option[u64]` when `ident` names a union
tuple_type := "#" "(" tuple_type_elem ("," tuple_type_elem)* ","? ")"
tuple_type_elem := (ident ":")? type
block    := "{" stmt* "}"
stmt     := binding_stmt | assign_stmt | index_assign_stmt | field_assign_stmt | tuple_assign_stmt | destructure_stmt | if_stmt | match_stmt | while_stmt | break_stmt | continue_stmt | return_stmt | expr_stmt
binding_stmt := ("var" | "val") (ident | destructure) (":" type)? "=" expr ";"
destructure_stmt := ("var" | "val") destructure (":" type)? "=" expr ";"
assign_stmt := ident "=" expr ";"
index_assign_stmt := assignable "[" expr "]" "=" expr ";"
field_assign_stmt := assignable "." ident "=" expr ";"
tuple_assign_stmt := assignable backtick_index "=" expr ";"
assignable := ident ("[" expr "]" | "." ident | backtick_index)*
backtick_index := "." "`" int   ; unnamed tuples only, e.g. t.`0
if_stmt  := "if" "(" expr ")" branch ("else" branch)?
match_stmt := "match" "(" expr ")" "{" match_arm* ("else" branch)? "}"
match_arm := (path ("(" ident ("," ident)* ","? ")")? | "null") block
           ; `path` is `Union.Variant` (2+ segments); bindings are implicit `val`s; `else` must be last
           ; `null` matches the empty case of a `?T` scrutinee (sugar for `Option.None`, no bindings)
while_stmt := "while" "(" expr ")" branch
break_stmt := "break" ";"
continue_stmt := "continue" ";"
return_stmt := "return" expr? ";"
branch   := block | stmt
expr_stmt := expr ";"                   ; mandatory; value discarded (no implicit return)
expr     := or
or       := and ("||" and)*
and      := equality ("&&" equality)*
equality := cast (("==" | "!=") cast)*
cast     := comparison ("as" type)*
comparison := term (("<" | "<=" | ">" | ">=") term)*
term     := factor (("+" | "-") factor)* ; left-assoc
factor   := unary (("*" | "/") unary)*   ; left-assoc
unary    := ("-" | "!") unary | postfix
call     := path ("::" "[" type ("," type)* "]")? "(" args? ")"
postfix  := primary ("[" expr "]" | "." ident | backtick_index)*
primary  := literal | string | "null" | array_literal | tuple_literal | object_literal | call | path | "(" expr ")"
         ; `null` is the empty value of any `?T` (sugar for the builtin `Option.None`; needs an annotation)
tuple_literal := "#" "(" tuple_elem ("," tuple_elem)* ","? ")"
tuple_elem := (ident "=")? expr
object_literal := ident "{" (ident "=" expr ("," ident "=" expr)* ","?)? "}"
array_literal := "[" (expr ("," expr)* ","?)? "]"
args     := expr ("," expr)*
literal  := int | i64 | u64 | f64 | u8 | bool
path     := ident ("." ident)*
```

Terminal names are `vl-lex` `TokenKind`s: `Var Val Fun Type Object Union Match If Else While Break
Continue Return As Extends Eq EqEq Bang BangEq Lt LtEq Gt GtEq AmpAmp PipePipe
Plus Minus Star Slash Semi LParen RParen LBrace RBrace LBracket RBracket Comma
Dot Colon Hash Backtick ColonColon Question Null Ident Int I64 U64 F64 U8 Bool String Invalid Eof`.

### Notes

* Semicolons are mandatory everywhere: `var`/`val`, object declarations, `return`, `break`, `continue`,
  and expression-statements need `;` — including the last statement of a
  fun body (`{ val d = x; }`). A bare trailing `d` without `;` is `E100`.
* There are no implicit returns: only `return expr;` yields a value
  (`return;` for `void`). A trailing `d;` is a discarded expression statement.
* Unary is `-` / `!`, right-recursive: `- -5`, `!x` ok; `+x` → `E103`.
  Casts use `as` and bind between arithmetic and equality: `a + b as u8`
  is `(a + b) as u8`, while `a == b as u8` is `a == (b as u8)`.
* Parens are transparent in the AST: `(e)` returns inner `Expr`, span drops parens.
* Calls are callee-by-name (`ident(args)`), so forward references to `fun` items work.
* Assignment statements are recognized from identifier-led postfix expressions;
  field and index writes may therefore chain postfix operations, while a
  parenthesized assignment base remains an expression-statement parse error.
* Tuples are fixed-arity heterogeneous values with copy semantics:
  `#(u64, String)` (unnamed) and `#(x: u64, y: String)` (named) need 2+
  elements, uniform named-ness, and no `void` (one diagnostic each).
  Literals mirror types with `=` (`#(1u64, "a")` / `#(x = 1u64)`).
  Unnamed access is backtick indexing (dot + backtick + bare int);
  named access is plain field. Destructuring is `val #(a, b) = t;` /
  `val #(x: x2) = u;` (rename via `field: binding`); element writes need
  a `*` tuple view.
* `*` in a type is a capability qualifier (`*Foo`, `*Array[T]`, `*#(...)`,
  `Array[*Foo]`); `*` in an expression stays multiplication (`a * b`).
  `mutable_type` goes through `type_atom` (not `type`), so `**Foo` is an
  immediate `E106`. Type spans include the leading `*`.
* Union payload parentheses are non-empty and do not allow a trailing payload comma:
  `Some(T)` is valid, while `Some()` and `Some(T,)` are rejected. Variant separators
  and the optional final union comma are accepted. Variants must begin with an uppercase
  letter and duplicate variant names produce one `E200`.
* Union types spell instantiations with brackets: `Option` (monomorphic) or
  `Option[u64]` (applied). Only names declared as `union` in the file parse
  this way; other `Name[...]` spellings are a typecheck error. Annotations for
  a generic union without arguments are rejected by typechecking (arity error).
  The builtin `Option` (backing `?T` / `null`) is available without a
  declaration; a local `type Option` shadows it.
* Nullable types (`?T`) desugar to the builtin `Option` union, so every later
  stage only sees unions ("sugar all the way"). `?` prefixes any `type`
  (`??T`, `Array[?u64]`, `?*Foo`); `?void` and a missing inner type are one
  `E104` each. A plain `T` value where `?T` is expected wraps as
  `Option.Some` implicitly; `null` is the empty case. `x == null` / `x != null`
  compare the discriminant tag; other `==` operands follow the usual
  comparability rules.
* Variant construction reuses call/field syntax: `Option.Some(1u64)` parses as
  a call and `Option.None` as a field path; both resolve to variants in later
  stages (an optional turbofish `Option.Some::[u64](...)` passes explicit args).
* `match` arms name `Union.Variant` (2+ path segments; a bare `Some(v)` is one
  `E100`), take an optional parenthesized binding list (implicit `val`s, one
  optional trailing comma), and require brace blocks. `else` takes a branch
  (like `if`) and must be the last arm (trailing arms after `else` are one
  `E100`). Typechecking requires `else` in this milestone; omitting it with
  uncovered variants is an exhaustiveness error.
* Objects declare associated functions inside the body
  (`type Counter = object { value: u64, fun bump(self: *Counter): *Counter { ... } };`).
  Fields and `fun` members share one namespace: a duplicate member name is one
  `E200` (`duplicate member`). The comma after a field may be omitted before a
  `fun` member or `}`; a trailing comma after a `fun` member is allowed but
  never required. Calls spell the owner explicitly (`Counter.bump(c)`); a
  `receiver.method(args)` call is accepted only when the method's first
  parameter takes the receiver's object type (checked by `vl-typecheck`).

## AST

```text
Program { items: Vec<Item> }
Item ::= Use { path, names, span }
       | Let { kind: Var | Val, name, name_span, ty, ty_span, value: Expr, span }
       | Destructure { kind: Var | Val, bindings: Vec<DestructureBinding>, bindings_span, ty, ty_span, value: Expr, span }
       | Function { name, name_span, type_params: Vec<TypeParam>, params: Vec<Param>, ret: Option<VlType>, ret_span, body: Vec<Stmt>, span }
       | Object { name, name_span, fields: Vec<ObjectField>, methods: Vec<AssociatedFn>, span }
       | Union { name, name_span, type_params: Vec<TypeParam>, variants: Vec<UnionVariant>, span }
UnionVariant ::= { name, name_span, payload: Vec<(VlType, Span)> }
AssociatedFn ::= { name, name_span, type_params: Vec<TypeParam>, params: Vec<Param>, ret: Option<VlType>, ret_span, body: Vec<Stmt>, span }
       ; same shape as Function; the owner lives on the enclosing Object
TypeParam ::= { name, span, bound: Option<GenericBound> }
Param ::= { name, name_span, ty: Option<VlType>, ty_span }
DestructureBinding ::= { field: Option<String>, field_span, binding: String, binding_span }
Stmt ::= Let { kind: Var | Val, name, name_span, ty, ty_span, value: Expr, span }
       | Assign { name, name_span, value: Expr, span }
       | IndexAssign { array, index, value, span }
       | FieldAssign { base, field, field_span, value, span }
       | TupleAssign { base, index: usize, index_span, value, span }
       | Destructure { kind: Var | Val, bindings: Vec<DestructureBinding>, bindings_span, ty, ty_span, value: Expr, span }
       | If { condition, then_body, else_body, span }
       | Match { scrutinee: Expr, arms: Vec<MatchArm>, else_body: Option<Vec<Stmt>>, span }
       | While { condition, body, span }
       | Break { span } | Continue { span }
       | Return { value: Option<Expr>, span }
       | Expr(Expr)
MatchArm ::= { path: Vec<String>, path_span, bindings: Vec<(String, Span)>, body: Vec<Stmt>, span }
Expr ::= Literal(Scalar, Span) | String(Vec<u8>, Span) | ArrayLiteral { elems, span }
         | TupleLiteral { elems: Vec<(Option<String>, Option<Span>, Expr)>, span }
         | TupleIndex { base, index: usize, index_span, span }
         | ObjectLiteral { name, name_span, fields: Vec<(String, Span, Expr)>, span }
         | Index { base, index, span }
         | Field { base, name, span } | Var { path, span }
         | Call { callee: path, callee_span, type_args, type_args_span, args, span }
         | Unary { op, rhs, span } | Binary { op, lhs, rhs, span }
         | Cast { inner, target, target_span, span }
BinOp ::= Add | Sub | Mul | Div | Eq | Ne | Lt | Le | Gt | Ge | And | Or
UnOp  ::= Neg | Not
```

The implementation names the two binding nodes `Item::Let` and `Stmt::Let`
for compatibility with older HIR/LIR code; their `kind` field is the source
keyword and is always `BindingKind::Var` or `BindingKind::Val`.

Spans (`vl_common::Span`, byte, half-open): `val` spans `val..;`, `fun` spans
`fun..}`, `Binary` spans `lhs.start..rhs.end`, `Unary` spans `minus.start..rhs.end`.

## Errors (all `Severity::Error`)

| Code | When | Message shape |
|---|---|---|
| `E100` | `expect()` mismatch (missing `= ; ( ) { }`) | `expected {what}, found {describe}` + label `unexpected token here` |
| `E101` | item doesn't start with `use`/`var`/`val`/`fun`/`type` | `expected an item (\`use\`, \`var\`, \`val\`, \`fun\` or \`type\`), found …` + label `items start with …` |
| `E102` | missing name (after `var`/`val`/`fun`, or bad param) | `expected a name, found …` + label `expected identifier here` |
| `E103` | bad expression start | `expected an expression, found …` + label `expected value here` |
| `E104` | missing param type / `void` param / expected type | `parameter \`{name}\` is missing a type` / `cannot be \`void\`` / `expected a type …` |
| `E105` | unknown type | `unknown type …` |
| `E106` | invalid mutable-view type (`*u64`, `*void`, `**Foo`, missing inner) | `` `*u64` is not a reference type `` / `` `*void` is not valid `` / `repeated capability qualifier` / `expected a type after `*`` |

`describe()`: `Ident(n)` → `` identifier `n` ``, `Int(v)` → `` integer `v` ``,
keywords/symbols backticked, `Eof` → `end of file`.
Missing `;` (`val x = 1`) → `E100`; `@` never reaches here (lexer `E000`).

## Recovery

* `program` loop: failed `parse_item()` → `recover_to_item_boundary`: skip
  until (and consuming) `;`/`}`, or stopping at `var`/`val`/`fun`/`type`/`Eof`.
* `fun` body loop: failed `parse_stmt()` → `recover_to_stmt_boundary`: skip
  until (and consuming) `;`, or stopping at `}`/`var`/`val`/`fun`/`if`/`while`/
  `break`/`continue`/`return`/`Eof`.
* `parse_expr/term` return `None` upward on missing rhs, so `1 +` abandons
  the whole item/stmt and recovers at the boundary. Poison rule (AGENTS.md):
  failed nodes are dropped, no cascading diag downstream.
* Malformed types (`*;`, `**Foo`, `*u64`) report one `E106`/`E104` and
  recover at the declaration boundary (`,`, `)`, `=`, `{`, `;`, `}`):
  a failed annotation with a missing follow drops the item/stmt so later
  items/statements still parse.

## Examples

```text
"val x = 1 + 2 * 3;"                → Item::Let(Val), Binary(Add, 1, Binary(Mul, 2, 3))
"fun main() { val d = x; }"    → Item::Function { params: [], body: [Let(Val, d)] }
"fun add(a: i64, b: i64): i64 { return a + b; }" → Item::Function { body: [Return(Binary(Add))] }
"fun main() { return; }"       → Item::Function { body: [Return(None)] }
"val x = 1"                         → E100 (expected `;`), item dropped
"fun main() { d }"             → E100 (expected `;`), stmt dropped
"fun f(): i64 { return 1 }"    → E100 (expected `;`), stmt dropped
"val x = - -5;"                     → Unary(Neg, Unary(Neg, 5))
"d;" at top level                   → E101 (items start with use/var/val/fun/type)
```

## Modules

Each source file is a module named after its filename without the `.vl`
extension. `use std.string;` brings the `string` module name into scope, but
not its exports, so members are written `string.len()`. Grouped imports bring
only listed exports into scope: `use std.fs.{open, read};`. A trailing export
can be imported directly: `use std.print;` behaves like `use std.{print};` and
brings `print` into scope.

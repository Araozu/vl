# VL v0 — Lexical Grammar (`vl-lex`)

Source: `crates/vl-lex/src/lib.rs`. Hand-rolled byte loop.
Signature: `lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>)`. Never panics.

## Top level

```text
source   := (trivia | token)* EOF
trivia   := whitespace | comment
token    := keyword | ident | number | bool | string | punct
EOF      := synthetic, zero-width span at src.len()
```

Lexer scans left-to-right on `src.as_bytes()`, index `i`.
At each step: whitespace → skip; `//` → skip to `\n`; else one token or one error.

## Trivia (no token emitted)

```text
whitespace := [ \t \r \n ]+
comment    := "//" [^\n]*
```

`//` wins over `/` (`Slash`). Comment runs to but not including `\n`.
The `\n` itself is then whitespace. Unterminated comment at EOF just stops.

## Tokens

```text
keyword := "let" | "function" | "type" | "object" | "if" | "else" | "while" | "break" | "continue" | "return"; exact match, else ident
ident   := [a-zA-Z_] [a-zA-Z0-9_]* ; stored as Ident(String)
number  := digits [ "." digits ] suffix?
suffix  := "u64" | "i64" | "f64" | "u8"
bool    := "true" | "false"
string  := `"` string_char* `"`  ; stored as String(Vec<u8>)
punct   := "+" | "-" | "*" | "/" | "=" | ";" | "(" | ")" | "{" | "}" | ","
```

`string_char` is any byte other than `"`, `\\`, `\n`, or `\r`, or one of the
escapes `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`, and `\\"`. String contents are
bytes; no UTF-8 decoding is performed.

| Spelling | `TokenKind` | Span |
|---|---|---|
| `let` | `Let` | `start..end` of word |
| `function` | `Function` | `start..end` of word |
| `type` | `Type` | `start..end` of word |
| `object` | `Object` | `start..end` of word |
| `if` | `If` | `start..end` of word |
| `else` | `Else` | `start..end` of word |
| `while` | `While` | `start..end` of word |
| `break` | `Break` | `start..end` of word |
| `continue` | `Continue` | `start..end` of word |
| `return` | `Return` | `start..end` of word |
| `[a-zA-Z_][a-zA-Z0-9_]*` | `Ident(String)` | `start..end` of word |
| `[0-9]+` | `Int(i64)` | `start..end` of digits |
| `[0-9]+u64` | `U64(u64)` | `start..end` |
| `[0-9]+i64` | `I64(i64)` | `start..end` |
| `[0-9]+u8` | `U8(u8)` | `start..end` |
| `[0-9]+.[0-9]+f64` | `F64(u64)` | `start..end`, using `f64::to_bits` |
| `true` / `false` | `Bool(bool)` | `start..end` of word |
| `"..."` | `String(Vec<u8>)` | `start..end` including quotes |
| `+` | `Plus` | `i..i+1` |
| `-` | `Minus` | `i..i+1` |
| `*` | `Star` | `i..i+1` |
| `/` | `Slash` | `i..i+1` (only if next byte isn't `/`) |
| `=` | `Eq` | `i..i+1` |
| `;` | `Semi` | `i..i+1` |
| `(` | `LParen` | `i..i+1` |
| `)` | `RParen` | `i..i+1` |
| `{` | `LBrace` | `i..i+1` |
| `}` | `RBrace` | `i..i+1` |
| `,` | `Comma` | `i..i+1` |

Notes:

* Maximal munch: `letx` → `Ident("letx")`, not `Let` + `Ident`. Same for `function1`, `letter`, `functional`.
* Ident continuation in code is `(b as char).is_alphanumeric() || b == b'_'` — for ASCII input this equals `[0-9A-Za-z_]`. Non-ASCII bytes fall through to the error arm.
* Unsuffixed integer literals are untyped `int`; a contextual concrete integer type is selected during type checking.
  Floating literals require the `f64` suffix.
* All spans are byte offsets, half-open `[start, end)` (`vl_common::Span`). Single-char punct is always length 1.
* `Eof` is always appended, even when errors occurred: `Span::empty(src.len())`.

## Errors (both `Severity::Error`, both recover by skipping)

| Code | Message | Span | Recovery |
|---|---|---|---|
| `E000` | `unexpected character \`{c}\`` + note `identifiers use letters, digits and \`_\`; see \`let\`, \`function\`` | `i..i+1` | emit diag, `i += 1`, no token |
| `E001` | invalid or out-of-range numeric literal | `start..i` of literal | emit diag, no token, continue after literal |
| `E002` | `unterminated string literal` | opening quote through line end/EOF | emit diag, no token; continue lexing |
| `E003` | `unknown string escape` | backslash and escaped byte | emit diag; no string token |

One diag per offending byte / per bad literal. Lexing never stops early.

## Examples

```text
"let x = 1 + 2;"  → Let Ident("x") Eq I64(1) Plus I64(2) Semi Eof, no diags
"// hi\nlet a=1;" → Let Ident("a") Eq I64(1) Semi Eof
"let x = @;"      → Let Ident("x") Eq Semi Eof + E000 on `@` (0-width? no: 1-byte span)
"1u8" → U8(1), no diags
"a\\n" → String([97, 10])
```

## Explicitly NOT lexed in v0

No `== != <= >= ! && ||`, no chars, no hex/binary/octal ints,
no block comments (`/* */`), no `_`-separators in ints (`1_000` → `I64(1)` +
`Ident("_000")` — `_000` starts with `_`, so it lexes as an identifier).

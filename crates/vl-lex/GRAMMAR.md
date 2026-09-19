# VL v0 — Lexical Grammar (`vl-lex`)

Source: `crates/vl-lex/src/lib.rs`. Hand-rolled byte loop.
Signature: `lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>)`. Never panics.

## Top level

```text
source   := (trivia | token)* EOF
trivia   := whitespace | comment
token    := keyword | ident | int | punct
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
keyword := "let" | "function"      ; exact match, else ident
ident   := [a-zA-Z_] [a-zA-Z0-9_]* ; stored as Ident(String)
int     := [0-9]+                  ; stored as Int(i64), see errors
punct   := "+" | "-" | "*" | "/" | "=" | ";" | "(" | ")" | "{" | "}" | ","
```

| Spelling | `TokenKind` | Span |
|---|---|---|
| `let` | `Let` | `start..end` of word |
| `function` | `Function` | `start..end` of word |
| `[a-zA-Z_][a-zA-Z0-9_]*` | `Ident(String)` | `start..end` of word |
| `[0-9]+` | `Int(i64)` | `start..end` of digits |
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
* `int` is digit-run then `lit.parse::<i64>()`. `99999999999999999999` lexes as zero tokens + error.
* All spans are byte offsets, half-open `[start, end)` (`vl_common::Span`). Single-char punct is always length 1.
* `Eof` is always appended, even when errors occurred: `Span::empty(src.len())`.

## Errors (both `Severity::Error`, both recover by skipping)

| Code | Message | Span | Recovery |
|---|---|---|---|
| `E000` | `unexpected character \`{c}\`` + note `identifiers use letters, digits and \`_\`; see \`let\`, \`function\`` | `i..i+1` | emit diag, `i += 1`, no token |
| `E001` | `integer literal out of range` + label `does not fit in i64` | `start..i` of digit run | emit diag, no token, continue after run |

One diag per offending byte / per bad literal. Lexing never stops early.

## Examples

```text
"let x = 1 + 2;"  → Let Ident("x") Eq Int(1) Plus Int(2) Semi Eof, no diags
"// hi\nlet a=1;" → Let Ident("a") Eq Int(1) Semi Eof
"let x = @;"      → Let Ident("x") Eq Semi Eof + E000 on `@` (0-width? no: 1-byte span)
"99999999999999999999" → Eof only + E001 over the whole run
```

## Explicitly NOT lexed in v0

No `== != <= >= ! && ||`, no strings/chars, no floats, no hex/binary/octal ints,
no block comments (`/* */`), no escapes, no `_`-separators in ints (`1_000` → `Int(1)` + `Ident("_000")` — wait, actually `_000` starts with `_` so it lexes as ident; beware).

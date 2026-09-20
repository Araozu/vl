# VL v0 — Lexical Grammar (`vl-lex`)

Source: `crates/vl-lex/src/lib.rs`. Hand-rolled byte loop.
Signature: `lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>)`. Never panics.

## Top level

```text
source   := (trivia | token)* EOF
trivia   := whitespace | comment
token    := keyword | ident | number | boolean | string | operator | delimiter
EOF      := synthetic, zero-width span at src.len()
```

The lexer scans left-to-right on `src.as_bytes()`. At each step it skips
whitespace or a line comment, then emits one token or one diagnostic and an
`Invalid` token. Two-character operators are recognized before their
single-character prefixes.

## Trivia (no token emitted)

```text
whitespace := [ \t \r \n ]+
comment    := "//" [^\n]*
```

`//` wins over `/` (`Slash`). Comments run to but do not include `\n`; the
newline is then whitespace. An unterminated comment at EOF simply stops.

## Tokens

```text
keyword    := "var" | "val" | "fun" | "type" | "object" | "if" | "else"
            | "while" | "break" | "continue" | "return" | "as" | "extends"
ident      := [a-zA-Z_] [a-zA-Z0-9_]*
number     := digits ("." digits)? suffix?
suffix     := "u64" | "i64" | "f64" | "u8"
boolean    := "true" | "false"
string     := `"` string_char* `"`
operator   := "==" | "!=" | "<=" | ">=" | "&&" | "||"
            | "+" | "-" | "*" | "/" | "=" | "!" | "<" | ">"
delimiter  := "::" | ";" | "(" | ")" | "{" | "}" | "[" | "]"
            | "," | "." | ":"
```

Keywords are exact word matches; all other words are `Ident(String)`. The
lexer recognizes `true` and `false` as `Bool`, rather than identifiers.
`use` is intentionally left as an identifier because the parser treats it as
a contextual item introducer.
`number` requires the `f64` suffix when a fractional part is present; integer
suffixes select the corresponding token kind. A numeric suffix is consumed as
ASCII alphanumerics, so an unknown suffix produces one `E001` diagnostic for
the whole literal.

`string_char` is any byte other than `"`, `\\`, `\n`, or `\r`, or one of the
escapes `\\0`, `\\n`, `\\r`, `\\t`, `\\\\`, and `\\"`. String contents are
bytes; no UTF-8 decoding is performed.

| Spelling | `TokenKind` | Span |
|---|---|---|
| `var`, `val`, `fun`, `type`, `object`, `if`, `else`, `while`, `break`, `continue`, `return` | matching keyword | word span |
| `as` | `As` | word span |
| `extends` | `Extends` | word span |
| `[a-zA-Z_][a-zA-Z0-9_]*` | `Ident(String)` | word span |
| `true` / `false` | `Bool(bool)` | word span |
| `[0-9]+` | `Int(i64)` | digit span |
| `[0-9]+i64` / `[0-9]+u64` / `[0-9]+u8` | `I64` / `U64` / `U8` | literal span |
| `[0-9]+.[0-9]+f64` | `F64(u64)` using `f64::to_bits` | literal span |
| `"..."` | `String(Vec<u8>)` | including quotes |
| `+ - * / =` | `Plus`, `Minus`, `Star`, `Slash`, `Eq` | one byte |
| `== != ! < <= > >=` | `EqEq`, `BangEq`, `Bang`, `Lt`, `LtEq`, `Gt`, `GtEq` | one or two bytes |
| `&& \|\|` | `AmpAmp`, `PipePipe` | two bytes |
| `; ( ) { } [ ] , . :` | matching delimiter | one byte |
| `::` | `ColonColon` | two bytes |
| malformed numeric/string or unexpected character | `Invalid` plus diagnostic | offending span |
| end of input | `Eof` | `Span::empty(src.len())` |

Notes:

* Maximal munch applies to words and operators: `varx` is `Ident("varx")`,
  and `::`, `==`, `!=`, `<=`, `>=`, `&&`, and `||` are single tokens.
* Identifier continuation in code is `b.is_ascii_alphanumeric() || b == b'_'`.
  For ASCII input this equals `[0-9A-Za-z_]`; non-ASCII bytes are errors.
* Unsuffixed integer literals are untyped `int`; contextual type selection
  happens during type checking.
* All spans are byte offsets, half-open `[start, end)` (`vl_common::Span`).
* `Eof` is always appended, even when errors occurred.
* `*` (`Star`) has no fixed meaning here: it is multiplication in
  expressions (`a * b`) and a capability qualifier in types (`*Foo`).
  The parser decides by position; the lexer emits `Star` in both cases.
  No `let`, `const`, `mut`, `&`, or dereference token exists.

## Errors (all `Severity::Error`, all recover by skipping)

| Code | Message | Span | Recovery |
|---|---|---|---|
| `E000` | `unexpected character \`{c}\`` | one character | emit `Invalid`, advance by the UTF-8 scalar width |
| `E001` | invalid or out-of-range numeric literal | whole literal | emit `Invalid`, continue after the literal |
| `E002` | `unterminated string literal` | opening quote through line end/EOF | emit `Invalid`, leave the newline for whitespace |
| `E003` | `unknown string escape` | backslash and escaped byte | emit `Invalid` string token and continue |

Lexing never stops early; later valid tokens and the final `Eof` are retained.

## Examples

```text
"var x = 1 + 2;"  → Var Ident("x") Eq Int(1) Plus Int(2) Semi Eof, no diags
"// hi\nvar a=1;" → Var Ident("a") Eq Int(1) Semi Eof
"val x = @;"      → Val Ident("x") Eq Invalid Semi Eof + E000 on `@`
"1u8"             → U8(1), no diags
"a\\n"             → String([97, 10])
"type Point = object { x: u64, };" → Type ... Object ... Colon ... Comma ...
"f::[u64](x)"     → Ident ColonColon LBracket Ident RBracket LParen ...
```

Unsupported v0 input includes character literals, block comments (`/* */`),
hex/binary/octal integers, and `_` separators in integers. For example,
`1_000` lexes as `Int(1)` followed by `Ident("_000")`.

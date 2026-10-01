// Tree-sitter grammar for the VL v0 surface language.
//
// Covers the full syntax grammar in `crates/vl-syntax/GRAMMAR.md`:
// objects, unions, error sets, tuples, nullable (`?T`) and fallible
// (`E!T`, `!T`) types, `match`, `try`/`catch`, tuple destructuring,
// backtick indexing, turbofish calls, and brace imports (`self` stays a
// plain identifier: it is contextual in VL, so it must not become a
// keyword token or `fun bump(self: ...)` stops parsing).
//
// Highlighting stays permissive where the compiler is strict (arity,
// exhaustiveness, uppercase variants, `else`-last): anything the real
// parser accepts must parse here; anything extra is harmless colour.

const PREC = {
  OR: 1,
  CATCH: 2,
  AND: 3,
  EQUALITY: 4,
  COMPARISON: 5,
  TERM: 6,
  FACTOR: 7,
  CAST: 8,
  UNARY: 9,
  POSTFIX: 10,
};

function commaSep(rule) {
  return optional(seq(rule, repeat(seq(',', rule)), optional(',')));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

function dottedName($) {
  return prec.left(seq(
    choice($.identifier, $.type_identifier),
    repeat1(seq('.', choice($.identifier, $.type_identifier))),
  ));
}

module.exports = grammar({
  name: 'vl',

  extras: $ => [$.comment, /\s/],
  word: $ => $.identifier,
  conflicts: $ => [
    [$.assignable, $.path],
  ],

  rules: {
    source_file: $ => repeat(choice(
      $.use_declaration,
      $.variable_declaration,
      $.function_declaration,
      $.type_declaration,
    )),

    use_declaration: $ => seq('use', $.module_path, ';'),

    variable_declaration: $ => seq(
      field('kind', $.binding_keyword),
      field('name', choice($.identifier, $.destructure_pattern)),
      optional(seq(':', field('type', $.type))),
      '=',
      field('value', $.expression),
      ';',
    ),

    // `val #(a, b) = t;` / `val #(x: x2) = u;`
    destructure_pattern: $ => seq(
      '#',
      '(',
      commaSep1($.destructure_binding),
      ')',
    ),
    destructure_binding: $ => seq(
      field('name', $.identifier),
      optional(seq(':', field('rename', $.identifier))),
    ),

    function_declaration: $ => seq(
      'fun',
      field('name', $.identifier),
      optional($.type_parameters),
      '(',
      optional($.parameters),
      ')',
      optional(seq(':', field('return_type', $.type))),
      field('body', $.block),
    ),

    type_declaration: $ => seq(
      'type',
      field('name', $.type_identifier),
      optional($.type_parameters),
      '=',
      field('body', choice(
        $.object_type,
        $.union_type,
        $.error_type,
      )),
      ';',
    ),

    type_parameters: $ => seq('[', commaSep1($.type_parameter), ']'),
    type_parameter: $ => seq(
      field('name', $.type_identifier),
      optional(seq('extends', field('bound', $.generic_bound))),
    ),
    generic_bound: $ => choice('Numeric', 'Comparable'),

    parameters: $ => commaSep1($.parameter),
    parameter: $ => seq(
      field('name', $.identifier),
      ':',
      field('type', $.type),
    ),

    object_type: $ => seq(
      'object',
      '{',
      repeat(seq($.object_member, optional(','))),
      '}',
    ),
    // Fields and associated `fun` members share one namespace; commas are
    // separators, optional before `fun` or `}` (highlighting stays permissive).
    object_member: $ => choice(
      $.object_field,
      $.function_declaration,
    ),
    object_field: $ => seq(
      field('name', $.identifier),
      ':',
      field('type', $.type),
    ),

    union_type: $ => seq(
      'union',
      '{',
      commaSep($.union_variant),
      '}',
    ),
    union_variant: $ => seq(
      field('name', $.type_identifier),
      optional(seq(
        '(',
        $.type,
        repeat(seq(',', $.type)),
        ')',
      )),
    ),

    error_type: $ => seq(
      'error',
      '{',
      commaSep($.error_variant),
      '}',
    ),
    error_variant: $ => seq(
      field('name', $.type_identifier),
      optional(seq(
        '(',
        $.type,
        repeat(seq(',', $.type)),
        ')',
      )),
    ),

    type: $ => choice(
      $.nullable_type,
      $.fallible_type,
      $.mutable_type,
      $.type_atom,
    ),
    // `?T` (nullable sugar over the builtin `Option` union).
    nullable_type: $ => prec.right(seq('?', field('inner', $.type))),
    // `!T` (inferred set) or `Io!T` (named set, possibly qualified
    // like `std.string.StringError!u8`).
    fallible_type: $ => prec.right(choice(
      seq('!', field('inner', $.type)),
      seq(
        field('set', choice(
          dottedName($),
          $.identifier,
          $.type_identifier,
        )),
        '!',
        field('inner', $.type),
      ),
    )),
    mutable_type: $ => seq('*', field('inner', choice($.type_atom, $.nullable_type))),
    type_atom: $ => choice(
      $.primitive_type,
      $.array_type,
      $.tuple_type,
      $.named_type,
      $.type_identifier,
    ),
    primitive_type: $ => choice(
      'u64', 'i64', 'f64', 'bool', 'u8', 'String', 'File', 'void',
    ),
    array_type: $ => seq('Array', '[', $.type, ']'),
    // `#(u64, String)` / `#(x: u64, y: String)`.
    tuple_type: $ => seq('#', '(', commaSep1($.tuple_type_element), ')'),
    tuple_type_element: $ => seq(
      optional(seq(field('name', $.identifier), ':')),
      field('type', $.type),
    ),
    // Qualified (`my_app.person.Person`, `std.string.StringError`) or
    // applied (`Option[u64]`) nominal types. A bare `Foo` stays a plain
    // `type_identifier`; the name is only `named_type` when dotted or
    // applied so single names keep their existing tree shape.
    named_type: $ => choice(
      seq(
        field('path', dottedName($)),
        optional(seq('[', commaSep1($.type), ']')),
      ),
      seq(
        field('name', choice($.identifier, $.type_identifier)),
        '[',
        commaSep1($.type),
        ']',
      ),
    ),

    block: $ => seq('{', repeat($.statement), '}'),
    statement: $ => choice(
      $.variable_declaration,
      $.assignment_statement,
      $.if_statement,
      $.match_statement,
      $.while_statement,
      $.break_statement,
      $.continue_statement,
      $.return_statement,
      $.expression_statement,
    ),

    assignment_statement: $ => seq(
      field('left', $.assignable),
      '=',
      field('right', $.expression),
      ';',
    ),
    assignable: $ => prec.left(PREC.POSTFIX, seq(
      $.identifier,
      repeat(choice(
        seq('[', $.expression, ']'),
        seq('.', $.identifier),
        $.backtick_index,
      )),
    )),
    // Unnamed tuple element access and assignment target: `t.`0`.
    backtick_index: $ => seq('.', '`', $.integer_literal),

    if_statement: $ => prec.right(seq(
      'if',
      '(',
      field('condition', $.expression),
      ')',
      field('consequence', $.branch),
      optional(seq('else', field('alternative', $.branch))),
    )),
    // `match (o) { Option.Some(v) { ... } null { ... } else { ... } }`.
    // Bindings are implicit `val`s; `null` matches the empty case of `?T`.
    match_statement: $ => seq(
      'match',
      '(',
      field('scrutinee', $.expression),
      ')',
      '{',
      repeat($.match_arm),
      optional(seq('else', field('alternative', $.branch))),
      '}',
    ),
    match_arm: $ => seq(
      field('pattern', choice($.path, $.null_literal)),
      optional($.match_bindings),
      field('body', $.block),
    ),
    match_bindings: $ => seq('(', commaSep($.identifier), ')'),
    while_statement: $ => seq(
      'while',
      '(',
      field('condition', $.expression),
      ')',
      field('body', $.branch),
    ),
    branch: $ => choice($.block, $.statement),
    break_statement: $ => seq('break', ';'),
    continue_statement: $ => seq('continue', ';'),
    return_statement: $ => seq('return', optional($.expression), ';'),
    expression_statement: $ => seq($.expression, ';'),

    expression: $ => choice(
      $.binary_expression,
      $.catch_expression,
      $.cast_expression,
      $.unary_expression,
      $.postfix_expression,
      $.primary_expression,
    ),
    binary_expression: $ => choice(
      prec.left(PREC.OR, seq($.expression, '||', $.expression)),
      prec.left(PREC.AND, seq($.expression, '&&', $.expression)),
      prec.left(PREC.EQUALITY, seq($.expression, choice('==', '!='), $.expression)),
      prec.left(PREC.COMPARISON, seq($.expression, choice('<', '<=', '>', '>='), $.expression)),
      prec.left(PREC.TERM, seq($.expression, choice('+', '-'), $.expression)),
      prec.left(PREC.FACTOR, seq($.expression, choice('*', '/'), $.expression)),
    ),
    // `expr catch fallback`: right-associative, binds tighter than `||`
    // (like the compiler; the fallback is a full `or` there, which no
    // highlighting grammar can distinguish, so this stays permissive).
    catch_expression: $ => prec.right(PREC.CATCH, seq(
      $.expression,
      'catch',
      $.expression,
    )),
    cast_expression: $ => prec.left(PREC.CAST, seq($.expression, 'as', $.type)),
    unary_expression: $ => prec(PREC.UNARY, seq(choice('-', '!', 'try'), $.expression)),
    postfix_expression: $ => prec.left(PREC.POSTFIX, seq(
      $.primary_expression,
      repeat(choice(
        seq('[', $.expression, ']'),
        seq('.', $.identifier),
        $.backtick_index,
        seq(optional($.type_arguments), '(', commaSep($.expression), ')'),
      )),
    )),

    type_arguments: $ => seq('::', '[', commaSep1($.type), ']'),
    primary_expression: $ => choice(
      $.literal,
      $.null_literal,
      $.string,
      $.array_literal,
      $.tuple_literal,
      $.object_literal,
      $.path,
      $.parenthesized_expression,
    ),
    null_literal: $ => 'null',
    parenthesized_expression: $ => seq('(', $.expression, ')'),
    array_literal: $ => seq('[', commaSep($.expression), ']'),
    // `#(1u64, "a")` / `#(x = 1u64, y = "b")`.
    tuple_literal: $ => seq('#', '(', commaSep($.tuple_element), ')'),
    tuple_element: $ => seq(
      optional(seq(field('name', $.identifier), '=')),
      field('value', $.expression),
    ),
    object_literal: $ => prec(10, seq(
      field('name', $.type_identifier),
      '{',
      optional(commaSep1($.object_initializer)),
      '}',
    )),
    object_initializer: $ => seq(
      field('name', $.identifier),
      '=',
      field('value', $.expression),
    ),
    // Dotted paths may start from a type name (`Counter.init`, `a.b.c`)
    // so namespaced and sugar calls highlight; middle segments may be
    // types (`std.string.StringError.OutOfBounds`). A bare `Type` stays
    // invalid VL but parses permissively here (highlighting only).
    // `Type { ... }` still parses as an object literal via the `{`
    // lookahead.
    path: $ => prec.left(seq(
      choice($.identifier, $.type_identifier),
      repeat(seq('.', choice($.identifier, $.type_identifier))),
    )),
    module_path: $ => seq(
      $.identifier,
      repeat(choice(
        seq('.', $.identifier),
        $.grouped_imports,
      )),
    ),
    // `use m.{Foo, Bar}` / `use m.{self, TcpError}`: `self` lexes as a
    // plain identifier (contextual) and `TcpError` as a type identifier.
    grouped_imports: $ => seq('.', '{', commaSep1(choice($.identifier, $.type_identifier)), '}'),

    literal: $ => choice(
      $.integer_literal,
      $.i64_literal,
      $.u64_literal,
      $.f64_literal,
      $.u8_literal,
      $.boolean,
    ),
    integer_literal: $ => token(/[0-9]+/),
    i64_literal: $ => token(prec(1, /[0-9]+i64/)),
    u64_literal: $ => token(prec(1, /[0-9]+u64/)),
    f64_literal: $ => token(prec(1, /[0-9]+\.[0-9]+f64/)),
    u8_literal: $ => token(prec(1, /[0-9]+u8/)),
    boolean: $ => choice('true', 'false'),
    string: $ => seq('"', repeat(choice($.escape_sequence, /[^"\\\n]/)), '"'),
    escape_sequence: $ => /\\[0nrt\\"]|\\./,

    binding_keyword: $ => choice('var', 'val'),
    identifier: $ => /[A-Za-z_][A-Za-z0-9_]*/,
    type_identifier: $ => token(prec(1, /[A-Z][A-Za-z0-9_]*/)),
    comment: $ => token(seq('//', /.*/)),
  },
});

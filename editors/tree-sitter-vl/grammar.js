// Tree-sitter grammar for the VL v0 surface language.

const PREC = {
  OR: 1,
  AND: 2,
  EQUALITY: 3,
  COMPARISON: 4,
  TERM: 5,
  FACTOR: 6,
  CAST: 7,
  UNARY: 8,
  POSTFIX: 9,
};

function commaSep(rule) {
  return optional(seq(rule, repeat(seq(',', rule)), optional(',')));
}

function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
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
      field('name', $.identifier),
      optional(seq(':', field('type', $.type))),
      '=',
      field('value', $.expression),
      ';',
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
      '=',
      field('body', $.object_type),
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
      optional($.object_fields),
      '}',
    ),
    object_fields: $ => commaSep1($.object_field),
    object_field: $ => seq(
      field('name', $.identifier),
      ':',
      field('type', $.type),
    ),

    type: $ => choice($.mutable_type, $.type_atom),
    mutable_type: $ => seq('*', field('inner', $.type_atom)),
    type_atom: $ => choice(
      $.primitive_type,
      $.array_type,
      $.type_identifier,
    ),
    primitive_type: $ => choice(
      'u64', 'i64', 'f64', 'bool', 'u8', 'String', 'File', 'void',
    ),
    array_type: $ => seq('Array', '[', $.type, ']'),

    block: $ => seq('{', repeat($.statement), '}'),
    statement: $ => choice(
      $.variable_declaration,
      $.assignment_statement,
      $.if_statement,
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
      )),
    )),

    if_statement: $ => prec.right(seq(
      'if',
      '(',
      field('condition', $.expression),
      ')',
      field('consequence', $.branch),
      optional(seq('else', field('alternative', $.branch))),
    )),
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
    cast_expression: $ => prec.left(PREC.CAST, seq($.expression, 'as', $.type)),
    unary_expression: $ => prec(PREC.UNARY, seq(choice('-', '!'), $.expression)),
    postfix_expression: $ => prec.left(PREC.POSTFIX, seq(
      $.primary_expression,
      repeat(choice(
        seq('[', $.expression, ']'),
        seq('.', $.identifier),
        seq(optional($.type_arguments), '(', commaSep($.expression), ')'),
      )),
    )),

    type_arguments: $ => seq('::', '[', commaSep1($.type), ']'),
    primary_expression: $ => choice(
      $.literal,
      $.string,
      $.array_literal,
      $.object_literal,
      $.path,
      $.parenthesized_expression,
    ),
    parenthesized_expression: $ => seq('(', $.expression, ')'),
    array_literal: $ => seq('[', commaSep($.expression), ']'),
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
    path: $ => prec.left(seq($.identifier, repeat(seq('.', $.identifier)))),
    module_path: $ => seq(
      $.identifier,
      repeat(choice(
        seq('.', $.identifier),
        $.grouped_imports,
      )),
    ),
    grouped_imports: $ => seq('.', '{', commaSep1($.identifier), '}'),

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

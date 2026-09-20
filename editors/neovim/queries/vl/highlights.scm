;; Query names follow the VL Tree-sitter grammar vocabulary.
;; The native syntax file remains the fallback until a parser is installed.

(comment) @comment
(string) @string
(integer_literal) @number
(float_literal) @number.float
(boolean) @boolean

(type_identifier) @type
(primitive_type) @type.builtin
(function_declaration name: (identifier) @function)
(function_call function: (identifier) @function.call)
(field_expression field: (identifier) @property)

(let_declaration "let" @keyword)
(function_declaration "fun" @keyword)
(type_declaration "type" @keyword)
(object_type "object" @keyword)
(if_statement "if" @keyword.conditional)
(else_clause "else" @keyword.conditional)
(while_statement "while" @keyword.repeat)
(return_statement "return" @keyword.return)
(break_statement "break" @keyword)
(continue_statement "continue" @keyword)

("=") @operator
("==") @operator
("!=") @operator
("<") @operator
("<=") @operator
(">") @operator
(">=") @operator
("+") @operator
("-") @operator
("*") @operator
("/") @operator
("&&") @operator
("||") @operator
("as") @keyword

(identifier) @variable

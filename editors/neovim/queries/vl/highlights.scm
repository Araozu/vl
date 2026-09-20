;; Query names follow the VL Tree-sitter grammar vocabulary.
;; The native syntax file remains the fallback until a parser is installed.

(comment) @comment
(string) @string
(integer_literal) @number
(f64_literal) @number.float
(boolean) @boolean

(type_identifier) @type
(primitive_type) @type.builtin
(function_declaration name: (identifier) @function)
(type_declaration name: (type_identifier) @type.definition)
(object_field name: (identifier) @property)
(object_initializer name: (identifier) @property)

(binding_keyword) @keyword
(function_declaration "fun" @keyword)
(type_declaration "type" @keyword)
(object_type "object" @keyword)
(if_statement "if" @keyword.conditional)
(if_statement "else" @keyword.conditional)
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

(path (identifier) @variable)

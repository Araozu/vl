;; Query names follow the VL Tree-sitter grammar vocabulary.
;; The native syntax file remains the fallback until a parser is installed.

(comment) @comment
(string) @string
(integer_literal) @number
(i64_literal) @number
(u64_literal) @number
(u8_literal) @number
(f64_literal) @number.float
(boolean) @boolean
(null_literal) @constant.builtin

(type_identifier) @type
(primitive_type) @type.builtin
(function_declaration name: (identifier) @function)
(type_declaration name: (type_identifier) @type.definition)
(object_field name: (identifier) @property)
(object_initializer name: (identifier) @property)
(union_variant name: (type_identifier) @type)
(error_variant name: (type_identifier) @type)
(tuple_type_element name: (identifier) @property)
(tuple_element name: (identifier) @property)
(destructure_binding name: (identifier) @variable)
(destructure_binding rename: (identifier) @variable)
(match_bindings (identifier) @variable.parameter)

(binding_keyword) @keyword
(use_declaration "use" @keyword.import)
(function_declaration "fun" @keyword)
(type_declaration "type" @keyword)
(object_type "object" @keyword)
(union_type "union" @keyword)
(error_type "error" @keyword)
(if_statement "if" @keyword.conditional)
(if_statement "else" @keyword.conditional)
(match_statement "match" @keyword.conditional)
(match_statement "else" @keyword.conditional)
(while_statement "while" @keyword.repeat)
(return_statement "return" @keyword.return)
(break_statement "break" @keyword)
(continue_statement "continue" @keyword)
(unary_expression "try" @keyword)
(catch_expression "catch" @keyword)

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
("?") @operator
("!") @operator
("#") @operator
("`") @operator
("::") @operator
("as") @keyword

(path (identifier) @variable)

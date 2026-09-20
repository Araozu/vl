;; Indent the bodies of declarations and control-flow constructs.

(function_declaration body: (block) @indent.begin)
(if_statement consequence: (block) @indent.begin)
(else_clause consequence: (block) @indent.begin)
(while_statement body: (block) @indent.begin)
(object_type body: (object_body) @indent.begin)

[("{") ("}")] @indent.branch

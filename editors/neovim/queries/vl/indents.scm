;; Indent the bodies of declarations and control-flow constructs.

(function_declaration body: (block) @indent.begin)
(if_statement consequence: (branch) @indent.begin)
(if_statement alternative: (branch) @indent.begin)
(match_statement alternative: (branch) @indent.begin)
(match_arm body: (block) @indent.begin)
(while_statement body: (branch) @indent.begin)
(object_type) @indent.begin
(union_type) @indent.begin
(error_type) @indent.begin

[("{") ("}")] @indent.branch

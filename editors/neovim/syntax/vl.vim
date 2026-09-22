if exists('b:current_syntax')
  finish
endif

syntax keyword vlDeclaration var val fun use type object extends
syntax keyword vlOperatorKeyword as
syntax keyword vlConditional if else
syntax keyword vlRepeat while
syntax keyword vlStatement break continue return
syntax keyword vlBoolean true false
syntax keyword vlNull null
syntax keyword vlBuiltinType u64 i64 f64 u8 bool void String File Array Numeric Comparable

syntax match vlTypeName '\<[A-Z][A-Za-z0-9_]*\>'
syntax match vlFunction '\<fun\>\s\+\zs[A-Za-z_][A-Za-z0-9_]*'
syntax match vlTypeDefinition '\<type\>\s\+\zs[A-Za-z_][A-Za-z0-9_]*'
syntax match vlFunctionCall '\<[A-Za-z_][A-Za-z0-9_]*\>\ze\s*\%(::\s*\[[^]]*\]\s*\)\?('
syntax match vlProperty '\.\zs[A-Za-z_][A-Za-z0-9_]*\>'

syntax match vlInvalidNumber '\<\d\+\%(\.\d\+\)\?[A-Za-z_]\w*\>'
syntax match vlInvalidNumber '\<\d\+\.\d\+\>'
syntax match vlFloat '\<\d\+\.\d\+f64\>'
syntax match vlNumber '\<\d\+\%(i64\|u64\|u8\)\?\>'

syntax match vlInvalidEscape +\\.+ contained
syntax match vlEscape +\\[0nrt\\"]+ contained
syntax region vlString start=+"+ skip=+\\\\\|\\"+ end=+"+ oneline contains=vlEscape,vlInvalidEscape

syntax keyword vlTodo TODO FIXME XXX NOTE contained
syntax match vlComment '//.*$' contains=vlTodo
syntax match vlOperator '\(==\|!=\|<=\|>=\|&&\|||\|::\|[+*/=!<>?-]\)'

highlight default link vlDeclaration Keyword
highlight default link vlOperatorKeyword Operator
highlight default link vlConditional Conditional
highlight default link vlRepeat Repeat
highlight default link vlStatement Statement
highlight default link vlBoolean Boolean
highlight default link vlNull Constant
highlight default link vlBuiltinType Type
highlight default link vlTypeName Type
highlight default link vlFunction Function
highlight default link vlTypeDefinition Type
highlight default link vlFunctionCall Function
highlight default link vlProperty Identifier
highlight default link vlNumber Number
highlight default link vlFloat Float
highlight default link vlInvalidNumber Error
highlight default link vlString String
highlight default link vlEscape SpecialChar
highlight default link vlInvalidEscape Error
highlight default link vlTodo Todo
highlight default link vlComment Comment
highlight default link vlOperator Operator

let b:current_syntax = 'vl'

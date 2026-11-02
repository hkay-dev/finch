(metavariable) @variable

(type_identifier) @type

(fragment_specifier) @type

(primitive_type) @type.builtin

(self) @variable.special

(field_identifier) @property

(shorthand_field_identifier) @property

(trait_item
  name: (type_identifier) @type.interface)

(impl_item
  trait: (type_identifier) @type.interface)

(abstract_type
  trait: (type_identifier) @type.interface)

(dynamic_type
  trait: (type_identifier) @type.interface)

(trait_bounds
  (type_identifier) @type.interface)

(call_expression
  function: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (field_expression
      field: (field_identifier) @function.method)
  ])

(generic_function
  function: [
    (identifier) @function
    (scoped_identifier
      name: (identifier) @function)
    (field_expression
      field: (field_identifier) @function.method)
  ])

(function_item
  name: (identifier) @function.definition)

(function_signature_item
  name: (identifier) @function.definition)

(macro_invocation
  macro: [
    (identifier) @function.special
    (scoped_identifier
      name: (identifier) @function.special)
  ])

(macro_invocation
  "!" @function.special)

(macro_definition
  name: (identifier) @function.special)

; Identifier conventions
; Assume all-caps names are constants
((identifier) @constant
  (#match? @constant "^_*[A-Z][A-Z\\d_]*$"))

; Assume uppercase names are types/enum-constructors
((identifier) @type
  (#match? @type "^[A-Z]"))

(identifier) @variable

; Ensure enum variants are highlighted correctly regardless of naming convention
(enum_variant
  name: (identifier) @variant)

; A variant in use: the CamelCase last part of a path (`Ordering::Less`, `Self::Open`), not an
; ALL_CAPS constant
(scoped_identifier
  name: (identifier) @variant
  (#match? @variant "^[A-Z][A-Za-z0-9]*[a-z][A-Za-z0-9]*$"))

; A CamelCase name in the middle of a longer path is a type (`component::Theme::update`)
(scoped_identifier
  path: (scoped_identifier
    name: (identifier) @type
    (#match? @type "^[A-Z]")))

; The end of a `use` path, alone, grouped or aliased, is a type when CamelCase and plain otherwise
(use_declaration
  argument: (scoped_identifier
    name: (identifier) @type
    (#match? @type "^[A-Z]")))

(use_list
  (scoped_identifier
    name: (identifier) @type
    (#match? @type "^[A-Z]")))

(use_list
  (identifier) @type
  (#match? @type "^[A-Z]"))

(use_as_clause
  path: (scoped_identifier
    name: (identifier) @type
    (#match? @type "^[A-Z]")))

[
  "("
  ")"
  "{"
  "}"
  "["
  "]"
] @punctuation.bracket

(_
  .
  "<" @punctuation.bracket
  ">" @punctuation.bracket)

[
  "."
  ";"
  ","
  "::"
] @punctuation.delimiter

"#" @punctuation.special

[
  "as"
  "async"
  "const"
  "default"
  "dyn"
  "enum"
  "extern"
  "fn"
  "impl"
  "let"
  "macro_rules!"
  "mod"
  "move"
  "pub"
  "raw"
  "ref"
  "static"
  "struct"
  "for"
  "trait"
  "type"
  "union"
  "unsafe"
  "use"
  "where"
  (crate)
  (mutable_specifier)
  (super)
] @keyword

[
  "await"
  "break"
  "continue"
  "else"
  "if"
  "in"
  "loop"
  "match"
  "return"
  "while"
  "yield"
] @keyword.control

(for_expression
  "for" @keyword.control)

[
  (string_literal)
  (raw_string_literal)
  (char_literal)
] @string

(escape_sequence) @string.escape

[
  (integer_literal)
  (float_literal)
] @number

(boolean_literal) @boolean

[
  (line_comment)
  (block_comment)
] @comment

[
  (line_comment
    (doc_comment))
  (block_comment
    (doc_comment))
] @comment.doc

[
  "!="
  "%"
  "%="
  "&"
  "&="
  "&&"
  "*"
  "*="
  "+"
  "+="
  "-"
  "-="
  "->"
  ".."
  "..="
  "..."
  "/="
  ":"
  "<<"
  "<<="
  "<"
  "<="
  "="
  "=="
  "=>"
  ">"
  ">="
  ">>"
  ">>="
  "@"
  "^"
  "^="
  "|"
  "|="
  "||"
  "?"
] @operator

; Avoid highlighting these as operators when used in doc comments.
(unary_expression
  "!" @operator)

operator: "/" @operator

(lifetime
  "'" @label
  (identifier) @label)

(parameter
  (identifier) @variable.parameter)

(attribute_item
  (attribute
    [
      (identifier) @attribute
      (scoped_identifier
        name: (identifier) @attribute)
      (token_tree
        (identifier) @attribute
        (#match? @attribute "^[a-z\\d_]*$"))
      (token_tree
        (identifier) @none
        "::"
        (#match? @none "^[a-z\\d_]*$"))
    ]))

(inner_attribute_item
  (attribute
    [
      (identifier) @attribute
      (scoped_identifier
        name: (identifier) @attribute)
      (token_tree
        (identifier) @attribute
        (#match? @attribute "^[a-z\\d_]*$"))
      (token_tree
        (identifier) @none
        "::"
        (#match? @none "^[a-z\\d_]*$"))
    ]))

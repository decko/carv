; TypeScript / TSX tree-sitter query file for carv.
; Captures definitions and references for structural tools.
;
; Based on tree-sitter-typescript 0.23.2 grammar node types.

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

; Function declarations and arrow functions assigned to variables.
(function_declaration
  name: (identifier) @name.definition.function) @definition.function

; Method definitions in classes / object literals.
(method_definition
  name: (property_identifier) @name.definition.method) @definition.method

; Class declarations.
(class_declaration
  name: (type_identifier) @name.definition.class) @definition.class

; Abstract class declarations.
(abstract_class_declaration
  name: (type_identifier) @name.definition.class) @definition.class

; Interface declarations.
(interface_declaration
  name: (type_identifier) @name.definition.interface) @definition.interface

; Variable declarations with arrow functions (const foo = () => {}).
(lexical_declaration
  (variable_declarator
    name: (identifier) @name.definition.function
    value: [
      (arrow_function)
      (function_expression)
    ]) @definition.function)

; Exported declarations.
(export_statement
  declaration: [
    (function_declaration
      name: (identifier) @name.definition.function) @definition.function
    (class_declaration
      name: (type_identifier) @name.definition.class) @definition.class
    (interface_declaration
      name: (type_identifier) @name.definition.interface) @definition.interface
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name.definition.function
        value: (arrow_function)) @definition.function)
  ])

; Type aliases.
(type_alias_declaration
  name: (type_identifier) @name.definition.type) @definition.type

; Enum declarations.
(enum_declaration
  name: (identifier) @name.definition.enum) @definition.enum

; ---------------------------------------------------------------------------
; References
; ---------------------------------------------------------------------------

; Function / method calls.
(call_expression
  function: [
    (identifier) @name.reference
    (member_expression
      property: (property_identifier) @name.reference)
  ]) @reference.call

; New expressions (class instantiation).
(new_expression
  constructor: (identifier) @name.reference) @reference.call

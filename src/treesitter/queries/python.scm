; Python tree-sitter query file for carv.
; Captures definitions and references for structural tools.
;
; Based on tree-sitter-python 0.25.0 grammar node types.

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

(function_definition
  name: (identifier) @name.definition.function) @definition.function

(class_definition
  name: (identifier) @name.definition.class) @definition.class

; Top-level constants and variables.
(module
  (expression_statement
    (assignment
      left: (identifier) @name.definition.constant)) @definition.constant)

; Method definitions inside a class body.
(class_definition
  body: (block
    (function_definition
      name: (identifier) @name.definition.method) @definition.method))

; Decorated functions / methods.
(decorated_definition
  definition: (function_definition
    name: (identifier) @name.definition.function) @definition.function)

(decorated_definition
  definition: (class_definition
    name: (identifier) @name.definition.class) @definition.class)

; ---------------------------------------------------------------------------
; References
; ---------------------------------------------------------------------------

; Function / method calls.
(call
  function: [
    (identifier) @name.reference
    (attribute
      attribute: (identifier) @name.reference)
  ]) @reference.call

; Import statements.
(import_statement
  name: (dotted_name
    (identifier) @name.reference)) @reference.call

(import_from_statement
  module_name: (dotted_name
    (identifier) @name.reference)) @reference.call

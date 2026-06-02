; Rust tree-sitter query file for carv.
; Captures definitions and references for structural tools (get_skeleton,
; get_function, replace_symbol).
;
; Based on tree-sitter-rust 0.24.2 grammar node types.

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

(function_item
  name: (identifier) @name.definition.function) @definition.function

; Method definitions (inside impl blocks).
(declaration_list
  (function_item
    name: (identifier) @name.definition.method) @definition.method)

; Struct definitions.
(struct_item
  name: (type_identifier) @name.definition.struct) @definition.struct

; Enum definitions.
(enum_item
  name: (type_identifier) @name.definition.struct) @definition.struct

; Union definitions.
(union_item
  name: (type_identifier) @name.definition.struct) @definition.struct

; Trait definitions.
(trait_item
  name: (type_identifier) @name.definition.trait) @definition.trait

; Impl blocks.
(impl_item
  trait: (_)? @name.definition.impl
  type: (type_identifier) @name.definition.impl) @definition.impl

; Type aliases.
(type_item
  name: (type_identifier) @name.definition.struct) @definition.struct

; Macro definitions.
(macro_definition
  name: (identifier) @name.definition.macro) @definition.macro

; Module definitions.
(mod_item
  name: (identifier) @name.definition.module) @definition.module

; Constant / static definitions.
(const_item
  name: (identifier) @name.definition.constant) @definition.constant

(static_item
  name: (identifier) @name.definition.constant) @definition.constant

; ---------------------------------------------------------------------------
; References
; ---------------------------------------------------------------------------

(call_expression
  function: (identifier) @name.reference) @reference.call

(call_expression
  function: (field_expression
    field: (field_identifier) @name.reference)) @reference.call

(macro_invocation
  macro: (identifier) @name.reference) @reference.call

; Field access — struct field references.
(field_expression
  field: (field_identifier) @name.reference) @reference.call

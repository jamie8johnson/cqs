;; Function declarations: foo x y = ...
(value_declaration
  (function_declaration_left
    (lower_case_identifier) @name)) @function

;; Type declarations: type Msg = Increment | Decrement
(type_declaration
  (upper_case_identifier) @name) @enum

;; Type aliases: type alias Model = { count : Int }
(type_alias_declaration
  (upper_case_identifier) @name) @typealias

;; Port declarations: port sendMessage : String -> Cmd msg
(port_annotation
  (lower_case_identifier) @name) @function

;; Module declaration (for file-level context)
(module_declaration
  (upper_case_qid) @name) @module

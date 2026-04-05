;; Classes, structs, actors (all use class_declaration with direct type_identifier)
;; post_process_chunk reclassifies based on keyword
(class_declaration
  (type_identifier) @name) @class

;; Extensions — name comes from user_type, not direct type_identifier
(class_declaration
  (user_type
    (type_identifier) @name)) @class

;; Protocols
(protocol_declaration
  (type_identifier) @name) @trait

;; Functions (top-level and methods)
(function_declaration
  (simple_identifier) @name) @function

;; Protocol function declarations (signatures without body)
(protocol_function_declaration
  (simple_identifier) @name) @function

;; Initializers (init declarations) — post_process reclassifies as Constructor
(init_declaration) @function

;; Typealias
(typealias_declaration
  (type_identifier) @name) @typealias

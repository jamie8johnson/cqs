;; Function declarations
(function_declaration
  name: (identifier) @name) @function

;; Container type assignments (const Point = struct { ... })
;; Reclassified to Struct/Enum/TypeAlias by post_process_zig
(variable_declaration
  (identifier) @name) @struct

;; Test declarations
(test_declaration) @function

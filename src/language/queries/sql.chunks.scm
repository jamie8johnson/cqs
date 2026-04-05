(create_function
  (object_reference) @name) @function

(create_procedure
  (object_reference) @name) @function

(alter_function
  (object_reference) @name) @function

(alter_procedure
  (object_reference) @name) @function

(create_view
  (object_reference) @name) @function

(create_trigger
  name: (identifier) @name) @function

;; Tables
(create_table
  (object_reference) @name) @struct

;; User-defined types
(create_type
  (object_reference) @name) @typealias

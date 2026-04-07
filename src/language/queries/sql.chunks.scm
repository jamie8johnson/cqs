;; Functions stay as Function (reusable, callable)
(create_function
  (object_reference) @name) @function

(alter_function
  (object_reference) @name) @function

;; Procedures, views, triggers → StoredProc
(create_procedure
  (object_reference) @name) @storedproc

(alter_procedure
  (object_reference) @name) @storedproc

(create_view
  (object_reference) @name) @storedproc

(create_trigger
  (object_reference) @name) @storedproc

;; Tables
(create_table
  (object_reference) @name) @struct

;; User-defined types
(create_type
  (object_reference) @name) @typealias

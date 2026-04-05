;; Function assignment: name <- function(...) or name = function(...)
(binary_operator
  lhs: (identifier) @name
  rhs: (function_definition)) @function

;; Non-function assignment: name <- expr (for constants and R6 classes)
;; post_process distinguishes R6Class calls (→ Class) from UPPER_CASE constants (→ Constant)
(binary_operator
  lhs: (identifier) @name
  rhs: (call)) @const

;; Scalar/literal assignment for constants: name <- 10, name <- "str", name <- TRUE
(binary_operator
  lhs: (identifier) @name
  rhs: (float)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (string)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (true)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (false)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (null)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (inf)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (nan)) @const

(binary_operator
  lhs: (identifier) @name
  rhs: (na)) @const

;; Negative literal: name <- -1
(binary_operator
  lhs: (identifier) @name
  rhs: (unary_operator)) @const

;; S4 class definition: setClass("ClassName", ...)
;; The actual class name is extracted from the first string argument in post_process.
(call
  function: (identifier) @name
  arguments: (arguments
    (argument
      (string)))) @class

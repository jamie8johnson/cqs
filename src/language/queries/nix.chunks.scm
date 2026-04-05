;; Attribute binding whose value is a function
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (function_expression)) @function

;; Attribute binding whose value is an attribute set
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (attrset_expression)) @struct

;; Attribute binding whose value is a recursive attribute set
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (rec_attrset_expression)) @struct

;; Attribute binding whose value is a function application (e.g., mkDerivation { ... })
(binding
  attrpath: (attrpath (identifier) @name)
  expression: (apply_expression)) @function

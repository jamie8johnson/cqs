;; Elements with start tag
(element
  (STag
    (Name) @name)) @struct

;; Self-closing elements
(element
  (EmptyElemTag
    (Name) @name)) @struct

;; Processing instructions (<?xml-stylesheet ... ?>)
(PI
  (PITarget) @name) @function

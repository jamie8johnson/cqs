;; Function definition: pub fn add(x: Int, y: Int) -> Int { ... }
(function
  name: (identifier) @name) @function

;; Custom type definition: pub type Color { Red Green Blue }
(type_definition
  (type_name
    name: (type_identifier) @name)) @struct

;; Type alias: pub type UserId = Int
(type_alias
  (type_name
    name: (type_identifier) @name)) @struct

;; Constant: pub const max_retries: Int = 3
(constant
  name: (identifier) @name) @const

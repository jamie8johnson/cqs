;; Class interfaces (@interface ... @end)
(class_interface
  (identifier) @name) @class

;; Class implementations (@implementation ... @end)
(class_implementation
  (identifier) @name) @class

;; Protocols (@protocol ... @end)
(protocol_declaration
  (identifier) @name) @interface

;; Method declarations (in @interface or @protocol — no body)
(method_declaration
  (identifier) @name) @function

;; Method definitions (in @implementation — with body)
(method_definition
  (identifier) @name) @function

;; C-style free functions
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @name)) @function

;; Properties with pointer types (@property NSString *name)
(property_declaration
  (struct_declaration
    (struct_declarator
      (pointer_declarator
        (identifier) @name)))) @property

;; Properties with value types (@property NSInteger age)
(property_declaration
  (struct_declaration
    (struct_declarator
      (identifier) @name))) @property

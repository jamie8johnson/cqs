;; Methods
(method
  name: (identifier) @name) @function

;; Singleton methods (def self.foo)
(singleton_method
  name: (identifier) @name) @function

;; Classes
(class
  name: (constant) @name) @class

;; Modules
(module
  name: (constant) @name) @module

;; Constants (UPPER_CASE = value)
(assignment
  left: (constant) @name) @const

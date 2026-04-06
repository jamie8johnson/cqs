;; Methods (Sub and Function)
(method_declaration name: (identifier) @name) @function

;; Constructors (Sub New — name field is "New")
(constructor_declaration) @function

;; Properties
(property_declaration name: (identifier) @name) @property

;; Fields
(field_declaration
  (variable_declarator (identifier) @name)) @property

;; Constants
(const_declaration
  (variable_declarator (identifier) @name)) @const

;; Events
(event_declaration name: (identifier) @name) @event

;; Delegates
(delegate_declaration name: (identifier) @name) @delegate

;; Types
(class_block name: (identifier) @name) @class
(module_block name: (identifier) @name) @module
(structure_block name: (identifier) @name) @struct
(interface_block name: (identifier) @name) @interface
(enum_block name: (identifier) @name) @enum

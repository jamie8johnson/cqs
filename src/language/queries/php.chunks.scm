;; Functions
(function_definition
  name: (name) @name) @function

;; Classes
(class_declaration
  name: (name) @name) @class

;; Interfaces
(interface_declaration
  name: (name) @name) @interface

;; Traits
(trait_declaration
  name: (name) @name) @trait

;; Enums (PHP 8.1+)
(enum_declaration
  name: (name) @name) @enum

;; Methods (reclassified to Method via method_containers when inside declaration_list)
(method_declaration
  name: (name) @name) @function

;; Constants
(const_declaration
  (const_element
    (name) @name)) @const

;; Namespaces
(namespace_definition
  name: (namespace_name) @name) @namespace

;; Properties
(property_declaration
  (property_element
    (variable_name) @name)) @property

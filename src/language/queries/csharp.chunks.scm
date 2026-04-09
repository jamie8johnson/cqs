;; Functions/methods
(method_declaration name: (identifier) @name) @function
(constructor_declaration name: (identifier) @name) @function
(operator_declaration) @function
(indexer_declaration) @function
(local_function_statement name: (identifier) @name) @function

;; Properties
(property_declaration name: (identifier) @name) @property

;; Delegates
(delegate_declaration name: (identifier) @name) @delegate

;; Events
(event_field_declaration
  (variable_declaration
    (variable_declarator (identifier) @name))) @event
(event_declaration name: (identifier) @name) @event

;; Types
(class_declaration name: (identifier) @name) @class
(struct_declaration name: (identifier) @name) @struct
(record_declaration name: (identifier) @name) @struct
(interface_declaration name: (identifier) @name) @interface
(enum_declaration name: (identifier) @name) @enum

;; Namespaces
(namespace_declaration
  name: (identifier) @name) @namespace

(namespace_declaration
  name: (qualified_name) @name) @namespace

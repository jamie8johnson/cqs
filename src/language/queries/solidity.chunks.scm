;; Contracts
(contract_declaration
  name: (identifier) @name
  body: (contract_body)) @class

;; Interfaces
(interface_declaration
  name: (identifier) @name
  body: (contract_body)) @interface

;; Libraries
(library_declaration
  name: (identifier) @name
  body: (contract_body)) @module

;; Structs
(struct_declaration
  name: (identifier) @name
  body: (struct_body)) @struct

;; Enums
(enum_declaration
  name: (identifier) @name
  body: (enum_body)) @enum

;; Functions
(function_definition
  name: (identifier) @name) @function

;; Modifiers (access control decorators)
(modifier_definition
  name: (identifier) @name) @modifier

;; Events
(event_definition
  name: (identifier) @name) @event

;; State variables
(state_variable_declaration
  name: (identifier) @name) @property

;; Errors (custom error types)
(error_declaration
  name: (identifier) @name) @struct

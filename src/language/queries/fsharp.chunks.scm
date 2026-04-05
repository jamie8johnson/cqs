;; Functions (let bindings with arguments)
(function_or_value_defn
  (function_declaration_left
    (identifier) @name)) @function

;; Module definitions
(module_defn
  (identifier) @name) @module

;; Type definitions — records → struct
(type_definition
  (record_type_defn
    (type_name
      type_name: (identifier) @name))) @struct

;; Type definitions — discriminated unions → enum
(type_definition
  (union_type_defn
    (type_name
      type_name: (identifier) @name))) @enum

;; Type definitions — enums → enum
(type_definition
  (enum_type_defn
    (type_name
      type_name: (identifier) @name))) @enum

;; Type definitions — interfaces
(type_definition
  (interface_type_defn
    (type_name
      type_name: (identifier) @name))) @interface

;; Type definitions — delegates
(type_definition
  (delegate_type_defn
    (type_name
      type_name: (identifier) @name))) @delegate

;; Type definitions — type abbreviations (type Foo = string)
(type_definition
  (type_abbrev_defn
    (type_name
      type_name: (identifier) @name))) @typealias

;; Type definitions — classes (anon_type_defn = class with optional primary constructor)
(type_definition
  (anon_type_defn
    (type_name
      type_name: (identifier) @name))) @class

;; Type extensions (type MyType with member ...)
(type_extension
  (type_name
    type_name: (identifier) @name)) @extension

;; Member definitions — concrete (member this.Method(...) = ...)
(member_defn
  (method_or_prop_defn
    name: (property_or_ident
      method: (identifier) @name))) @function

;; Member definitions — abstract (abstract member Name: ...)
(member_defn
  (member_signature
    (identifier) @name)) @function

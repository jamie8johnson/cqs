(function_declaration
  name: (identifier) @name) @function

(method_declaration
  name: (field_identifier) @name) @function

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (struct_type))) @struct

(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (interface_type))) @interface

;; Type aliases — named types (type MyInt int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (type_identifier))) @typealias

;; Type aliases — function types (type Handler func(...))
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (function_type))) @typealias

;; Type aliases — pointer types (type Ptr *int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (pointer_type))) @typealias

;; Type aliases — slice types (type Names []string)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (slice_type))) @typealias

;; Type aliases — map types (type Cache map[string]int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (map_type))) @typealias

;; Type aliases — array types (type Data [10]byte)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (array_type))) @typealias

;; Type aliases — channel types (type Ch chan int)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (channel_type))) @typealias

;; Type aliases — qualified types (type Foo pkg.Type)
(type_declaration
  (type_spec
    name: (type_identifier) @name
    type: (qualified_type))) @typealias

;; Go 1.9+ type alias (type Foo = int)
(type_declaration
  (type_alias
    name: (type_identifier) @name)) @typealias

(const_declaration
  (const_spec
    name: (identifier) @name)) @const

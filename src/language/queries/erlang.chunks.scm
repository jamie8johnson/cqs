;; Function declaration (the outermost form wrapping function_clause(s))
(fun_decl
  clause: (function_clause
    name: (atom) @name)) @function

;; Module attribute: -module(name).
(module_attribute
  name: (atom) @name) @struct

;; Type alias: -type name(...) :: ...
(type_alias
  name: (type_name
    name: (atom) @name)) @struct

;; Opaque type: -opaque name(...) :: ...
(opaque
  name: (type_name
    name: (atom) @name)) @struct

;; Record declaration: -record(name, {fields}).
(record_decl
  name: (atom) @name) @struct

;; Behaviour attribute: -behaviour(name).
(behaviour_attribute
  name: (atom) @name) @interface

;; Callback: -callback name(Args) -> Ret.
(callback
  fun: (atom) @name) @interface

;; Preprocessor macro: -define(NAME, value).
(pp_define
  lhs: (macro_lhs
    name: (var) @name)) @macro

(pp_define
  lhs: (macro_lhs
    name: (atom) @name)) @macro

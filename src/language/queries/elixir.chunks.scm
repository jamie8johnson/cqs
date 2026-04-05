;; Function with arguments: def foo(args) do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (call
      target: (identifier) @name))
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp" "defdelegate")) @function

;; Function with guard: def foo(args) when guard do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (binary_operator
      left: (call
        target: (identifier) @name)))
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp")) @function

;; Zero-arity function: def foo do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (identifier) @name)
  (#any-of? @_keyword "def" "defp" "defmacro" "defmacrop" "defguard" "defguardp" "defdelegate")) @function

;; Module definition: defmodule MyApp.Foo do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (alias) @name)
  (#any-of? @_keyword "defmodule" "defprotocol")) @struct

;; defimpl: defimpl Protocol, for: Type do ... end
(call
  target: (identifier) @_keyword
  (arguments
    (alias) @name)
  (#eq? @_keyword "defimpl")) @struct

;; defstruct: defstruct [:field1, :field2]
(call
  target: (identifier) @_keyword
  (#eq? @_keyword "defstruct")) @struct

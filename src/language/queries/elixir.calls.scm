;; Local function call: foo(args)
(call
  target: (identifier) @callee
  (#not-any-of? @callee "def" "defp" "defmodule" "defprotocol" "defimpl" "defmacro" "defmacrop" "defstruct" "defguard" "defguardp" "defdelegate" "defexception" "defoverridable" "use" "import" "require" "alias"))

;; Remote function call: Module.function(args)
(call
  target: (dot
    right: (identifier) @callee))

;; Pipe into function: data |> function
(binary_operator
  operator: "|>"
  right: (identifier) @callee)

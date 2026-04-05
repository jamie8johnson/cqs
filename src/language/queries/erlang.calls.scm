;; Local function call: foo(args)
(call
  expr: (atom) @callee)

;; Remote function call: module:function(args)
(call
  expr: (remote
    fun: (atom) @callee))

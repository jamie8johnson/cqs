;; Member function call — token.transfer() → captures "transfer"
(member_expression
  property: (identifier) @callee)

;; All function calls — captures the full callee expression
;; For direct calls like require(), this captures "require"
;; For member calls, this captures "token.transfer" (deduped with above)
(call_expression
  function: (_) @callee)

;; Command calls: Get-Process, Invoke-WebRequest, etc.
(command
  command_name: (command_name) @callee)

;; .NET method invocations: $obj.Method()
;; Note: grammar uses "invokation" (typo in grammar, not our code)
(invokation_expression
  (member_name
    (simple_name) @callee))

;; Member access: $obj.Property or [Type]::StaticMethod
(member_access
  (member_name
    (simple_name) @callee))

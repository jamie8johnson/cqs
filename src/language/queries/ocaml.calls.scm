;; Function application: foo x or Module.func x
;; All calls go through value_path (even unqualified ones)
(application_expression
  function: (value_path
    (value_name) @callee))

;; Function application — first child is the function
(application_expression
  . (long_identifier_or_op
       (long_identifier
         (identifier) @callee)))

;; Dot access calls — obj.Method
(dot_expression
  field: (long_identifier_or_op
           (long_identifier
             (identifier) @callee)))

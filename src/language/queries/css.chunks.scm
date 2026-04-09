;; Rule set: .class { color: red; }
(rule_set
  (selectors) @name) @property

;; Keyframes: @keyframes spin { ... }
(keyframes_statement
  (keyframes_name) @name) @property

;; Media query: @media (max-width: 600px) { ... }
(media_statement) @property

;; Supports: @supports (display: grid) { ... }
(supports_statement) @property

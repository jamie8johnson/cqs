;; Tables ([section])
(table
  (bare_key) @name) @property

;; Tables with dotted keys ([section.subsection])
(table
  (dotted_key) @name) @property

;; Tables with quoted keys (["section"])
(table
  (quoted_key) @name) @property

;; Table arrays ([[array]])
(table_array_element
  (bare_key) @name) @property

;; Table arrays with dotted keys ([[array.sub]])
(table_array_element
  (dotted_key) @name) @property

;; Top-level key-value pairs
(pair
  (bare_key) @name) @property

;; Top-level dotted key-value pairs
(pair
  (dotted_key) @name) @property

;; Top-level quoted key-value pairs
(pair
  (quoted_key) @name) @property

;; Tables ([section])
(table
  (bare_key) @name) @configkey

;; Tables with dotted keys ([section.subsection])
(table
  (dotted_key) @name) @configkey

;; Tables with quoted keys (["section"])
(table
  (quoted_key) @name) @configkey

;; Table arrays ([[array]])
(table_array_element
  (bare_key) @name) @configkey

;; Table arrays with dotted keys ([[array.sub]])
(table_array_element
  (dotted_key) @name) @configkey

;; Top-level key-value pairs
(pair
  (bare_key) @name) @configkey

;; Top-level dotted key-value pairs
(pair
  (dotted_key) @name) @configkey

;; Top-level quoted key-value pairs
(pair
  (quoted_key) @name) @configkey

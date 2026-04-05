;; Regular elements — name is the tag
(element
  (start_tag
    (tag_name) @name)) @property

;; Self-closing elements
(element
  (self_closing_tag
    (tag_name) @name)) @property

;; Script blocks
(script_element
  (start_tag
    (tag_name) @name)) @property

;; Style blocks
(style_element
  (start_tag
    (tag_name) @name)) @property

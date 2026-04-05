;; Regular elements
(element
  (start_tag (tag_name) @name)) @property

;; Self-closing elements
(element
  (self_closing_tag (tag_name) @name)) @property

;; Script blocks (outer chunk replaced by JS injection)
(script_element) @module

;; Style blocks (outer chunk replaced by CSS injection)
(style_element) @module

;; Template blocks
(template_element
  (start_tag (tag_name) @name)) @property

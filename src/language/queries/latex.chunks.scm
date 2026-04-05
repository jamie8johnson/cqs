;; Part
(part
  text: (curly_group) @name) @section

;; Chapter
(chapter
  text: (curly_group) @name) @section

;; Section
(section
  text: (curly_group) @name) @section

;; Subsection
(subsection
  text: (curly_group) @name) @section

;; Subsubsection
(subsubsection
  text: (curly_group) @name) @section

;; Paragraph (LaTeX \paragraph{})
(paragraph
  text: (curly_group) @name) @section

;; New command definitions (declaration in curly group)
(new_command_definition
  declaration: (curly_group_command_name) @name) @function

;; New command definitions (bare command name)
(new_command_definition
  declaration: (command_name) @name) @function

;; Old-style command definitions (\def)
(old_command_definition
  declaration: (command_name) @name) @function

;; Named environments
(generic_environment
  begin: (begin
    name: (curly_group_text) @name)) @struct

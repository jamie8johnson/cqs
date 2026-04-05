;; Function definition: foo x y = ...
(function
  name: (variable) @name) @function

;; Data type definition: data Foo = Bar | Baz
(data_type
  name: (name) @name) @struct

;; Newtype definition: newtype Foo = Foo a
(newtype
  name: (name) @name) @struct

;; Type synonym: type Foo = Bar
(type_synomym
  name: (name) @name) @struct

;; Typeclass definition: class Foo a where ...
(class
  name: (name) @name) @trait

;; Instance declaration: instance Foo Bar where ...
(instance
  name: (name) @name) @struct

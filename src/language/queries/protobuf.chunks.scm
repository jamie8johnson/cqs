;; Messages
(message
  (message_name
    (identifier) @name)) @struct

;; Services
(service
  (service_name
    (identifier) @name)) @interface

;; RPCs (inside services → Method via method_containers)
(rpc
  (rpc_name
    (identifier) @name)) @function

;; Enums
(enum
  (enum_name
    (identifier) @name)) @enum

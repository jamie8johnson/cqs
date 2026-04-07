;; Messages
(message
  (message_name
    (identifier) @name)) @struct

;; Services → Service type (contract definitions, not implementations)
(service
  (service_name
    (identifier) @name)) @service

;; RPCs (inside services → Method via method_containers)
(rpc
  (rpc_name
    (identifier) @name)) @function

;; Enums
(enum
  (enum_name
    (identifier) @name)) @enum

module Sample

let add a b = a + b

let multiply a b = a * b

type Shape =
    | Circle of float
    | Rectangle of float * float

let area shape =
    match shape with
    | Circle r -> System.Math.PI * r * r
    | Rectangle (w, h) -> w * h

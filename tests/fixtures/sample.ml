(* Calculator module *)
module Calculator = struct
  let add x y = x + y

  let multiply x y = x * y

  let factorial n =
    let rec aux acc = function
      | 0 -> acc
      | n -> aux (acc * n) (n - 1)
    in
    aux 1 n
end

(* Variant type for colors *)
type color = Red | Green | Blue

(* Record type for 2D points *)
type point = {
  x : float;
  y : float;
}

(* Type alias *)
type name = string

let greet name =
  Printf.printf "Hello, %s!\n" name

let distance p1 p2 =
  let dx = p1.x -. p2.x in
  let dy = p1.y -. p2.y in
  sqrt (dx *. dx +. dy *. dy)

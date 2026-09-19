(* Three nested loops over flat float arrays: the shape of numerical code, and
   of nothing else. `i k j` order, so the innermost loop walks in order.

   `float array` is unboxed in OCaml, so this is a flat buffer of doubles. *)

let n = 256

let () =
  let a = Array.make (n * n) 0.0 in
  let b = Array.make (n * n) 0.0 in
  let c = Array.make (n * n) 0.0 in
  for i = 0 to n - 1 do
    for j = 0 to n - 1 do
      a.((i * n) + j) <- float_of_int ((i + j) mod 10);
      b.((i * n) + j) <- float_of_int (i * j mod 10)
    done
  done;
  for i = 0 to n - 1 do
    for k = 0 to n - 1 do
      let aik = a.((i * n) + k) in
      for j = 0 to n - 1 do
        c.((i * n) + j) <- c.((i * n) + j) +. (aik *. b.((k * n) + j))
      done
    done
  done;
  let total = ref 0.0 in
  for i = 0 to n - 1 do
    total := !total +. c.((i * n) + i)
  done;
  print_int (int_of_float !total);
  print_newline ()

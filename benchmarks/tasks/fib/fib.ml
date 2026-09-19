(* fib n, the naive way: two calls and an addition per node, nothing else. *)

let rec fib n = if n < 2 then n else fib (n - 1) + fib (n - 2)

let () = print_int (fib 32); print_newline ()

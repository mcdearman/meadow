(* Allocation and collection: build a small tree, walk it, discard it, tens of
   millions of nodes over, with one long-lived tree kept alive throughout.

   Each tip carries the iteration it was built in, so no two trees are alike
   and no compiler can build one and reuse it. *)

type tree = Tip of int | Fork of tree * tree

let rec build v d =
  if d <= 0 then Tip v else Fork (build v (d - 1), build (v + 1) (d - 1))

let rec check = function Tip v -> v | Fork (l, r) -> check l + check r

let () =
  let top = 18 in
  let lasting = build 1 top in
  let total = ref 0 in
  let d = ref 4 in
  while !d <= top do
    for i = 1 lsl (top - !d + 4) downto 1 do
      total := !total + check (build i !d)
    done;
    d := !d + 2
  done;
  print_int (!total + check lasting);
  print_newline ()

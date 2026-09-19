(* Allocation and collection: build a small tree, walk it, discard it, tens of
   millions of nodes over, with one long-lived tree kept alive throughout.

   Each tip carries the iteration it was built in, so no two trees are alike
   and no compiler can build one and reuse it. *)

datatype tree = Tip of int | Fork of tree * tree

fun build (v : int) (d : int) : tree =
  if d <= 0 then Tip v else Fork (build v (d - 1), build (v + 1) (d - 1))

fun check (Tip v) = v
  | check (Fork (l, r)) = check l + check r

fun trees d 0 acc = acc
  | trees d n acc = trees d (n - 1) (acc + check (build n d))

fun depths d top acc =
  if d > top then acc
  else depths (d + 2) top (acc + trees d (IntInf.toInt (IntInf.pow (2, top - d + 4))) 0)

val () =
  let
    val top = 18
    val lasting = build 1 top
  in
    print (Int.toString (depths 4 top 0 + check lasting) ^ "\n")
  end

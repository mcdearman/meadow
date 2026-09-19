// Allocation and collection: build a small tree, walk it, discard it, tens of
// millions of nodes over, with one long-lived tree kept alive throughout.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

type tree
  Tip(value : int)
  Fork(left : tree, right : tree)

fun build(v : int, d : int) : div tree
  if d <= 0 then Tip(v) else Fork(build(v, d - 1), build(v + 1, d - 1))

fun check(t : tree) : div int
  match t
    Tip(v) -> v
    Fork(l, r) -> check(l) + check(r)

fun trees(d : int, n : int, acc : int) : div int
  if n == 0 then acc else trees(d, n - 1, acc + check(build(n, d)))

fun depths(d : int, top : int, acc : int) : div int
  if d > top then acc
  else depths(d + 2, top, acc + trees(d, 2^(top - d + 4), 0))

fun main()
  val top = 18
  val lasting = build(1, top)
  println((depths(4, top, 0) + check(lasting)).show)

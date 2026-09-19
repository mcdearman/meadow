// Three nested loops over flat vectors of doubles: the shape of numerical code,
// and of nothing else. `i k j` order, so the innermost loop walks in order.

import std/num/float64

val n = 256

fun build(f : (int, int) -> float64) : vector<float64>
  vector-init(n * n, fn(p) f(p / n, p % n))

fun main()
  val a = build(fn(i, j) ((i + j) % 10).float64)
  val b = build(fn(i, j) ((i * j) % 10).float64)
  var c := vector-init(n * n, fn(_) 0.0)
  for(0, n - 1) fn(i)
    for(0, n - 1) fn(k)
      val aik = a[i * n + k]
      for(0, n - 1) fn(j)
        c[i * n + j] := c[i * n + j] + aik * b[k * n + j]
  var total := 0.0
  for(0, n - 1) fn(i)
    total := total + c[i * n + i]
  println(total.int.show)

// fib n, the naive way: two calls and an addition per node, nothing else.

fun fib(n : int) : div int
  if n < 2 then n else fib(n - 1) + fib(n - 2)

fun main()
  println(fib(32).show)

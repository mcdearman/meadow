(* Three nested loops over flat float arrays: the shape of numerical code, and
   of nothing else. `i k j` order, so the innermost loop walks in order.

   `Real64Array` is an unboxed array of doubles. *)

val n = 256

fun loop (lo : int) hi f = if lo >= hi then () else (f lo; loop (lo + 1) hi f)

val () =
  let
    val a = Real64Array.array (n * n, 0.0)
    val b = Real64Array.array (n * n, 0.0)
    val c = Real64Array.array (n * n, 0.0)
  in
    loop 0 n (fn i =>
      loop 0 n (fn j =>
        (Real64Array.update (a, i * n + j, real ((i + j) mod 10));
         Real64Array.update (b, i * n + j, real ((i * j) mod 10)))));
    loop 0 n (fn i =>
      loop 0 n (fn k =>
        let val aik = Real64Array.sub (a, i * n + k) in
          loop 0 n (fn j =>
            Real64Array.update (c, i * n + j,
              Real64Array.sub (c, i * n + j) + aik * Real64Array.sub (b, k * n + j)))
        end));
    let
      val total = ref 0.0
    in
      loop 0 n (fn i => total := !total + Real64Array.sub (c, i * n + i));
      print (LargeInt.toString (Real.toLargeInt IEEEReal.TO_ZERO (!total)) ^ "\n")
    end
  end

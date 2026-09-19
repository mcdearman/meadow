-- fib n, the naive way: two calls and an addition per node, nothing else.
--
-- `Int` rather than the defaulted `Integer`, which is a different benchmark.

fib :: Int -> Int
fib n = if n < 2 then n else fib (n - 1) + fib (n - 2)

main :: IO ()
main = print (fib 32)

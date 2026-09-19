-- Allocation and collection: build a small tree, walk it, discard it, tens of
-- millions of nodes over, with one long-lived tree kept alive throughout.
--
-- Strict fields: a lazy tree would not be built until it was walked, which
-- measures something else entirely.
--
-- Each tip carries the iteration it was built in, so no two trees are alike.
-- Without that GHC floats the tree out of the loop and builds it once, and
-- the benchmark finishes in a fiftieth of the time having measured nothing.

import Data.Bits (shiftL)
import Data.List (foldl')

data Tree = Tip !Int | Fork !Tree !Tree

build :: Int -> Int -> Tree
build v d = if d <= 0 then Tip v else Fork (build v (d - 1)) (build (v + 1) (d - 1))

check :: Tree -> Int
check (Tip v) = v
check (Fork l r) = check l + check r

trees :: Int -> Int -> Int -> Int
trees _ 0 acc = acc
trees d n acc = trees d (n - 1) (acc + check (build n d))

main :: IO ()
main = do
  let top = 18
      lasting = build 1 top
      total = foldl' (\a d -> a + trees d (1 `shiftL` (top - d + 4)) 0) 0 [4, 6 .. top]
  print (total + check lasting)

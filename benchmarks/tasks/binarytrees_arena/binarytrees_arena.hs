-- `binarytrees`, with an arena: the same trees, built into one flat block of
-- nodes and thrown away whole.
--
-- A node is three slots -- value, left, right -- and a child is the index of
-- one, not a constructor. Building a tree is a bump of a counter per node;
-- discarding one is setting the counter back to nought. What is left in the
-- number is writing the nodes and walking them, with the allocator and the
-- collector taken out.
--
-- Each tip carries the iteration it was built in, so no two trees are alike.

import Control.Monad.ST (ST, runST)
import Data.Array.Base (unsafeRead, unsafeWrite)
import Data.Array.ST (STUArray, newArray)
import Data.Bits (shiftL)
import Data.STRef (STRef, newSTRef, readSTRef, writeSTRef)

tip :: Int
tip = -1

build :: STUArray s Int Int -> STRef s Int -> Int -> Int -> ST s Int
build a n v d = do
  i <- readSTRef n
  writeSTRef n (i + 3)
  if d <= 0
    then do
      unsafeWrite a i v
      unsafeWrite a (i + 1) tip
      pure i
    else do
      l <- build a n v (d - 1)
      r <- build a n (v + 1) (d - 1)
      unsafeWrite a i 0
      unsafeWrite a (i + 1) l
      unsafeWrite a (i + 2) r
      pure i

check :: STUArray s Int Int -> Int -> ST s Int
check a i = do
  l <- unsafeRead a (i + 1)
  if l == tip
    then unsafeRead a i
    else do
      cl <- check a l
      r <- unsafeRead a (i + 2)
      cr <- check a r
      pure (cl + cr)

main :: IO ()
main = print (runST go)
  where
    top = 18
    -- Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots each.
    room = 3 * (1 `shiftL` (top + 1))
    go :: ST s Int
    go = do
      lasting <- newArray (0, room - 1) 0
      ln <- newSTRef 0
      root <- build lasting ln 1 top
      arena <- newArray (0, room - 1) 0
      bump <- newSTRef 0
      let trees d n acc
            | n == 0 = pure acc
            | otherwise = do
                writeSTRef bump 0
                t <- build arena bump n d
                c <- check arena t
                trees d (n - 1) (acc + c)
          depths d acc
            | d > top = pure acc
            | otherwise = do
                s <- trees d (1 `shiftL` (top - d + 4)) 0
                depths (d + 2) (acc + s)
      total <- depths 4 0
      c <- check lasting root
      pure (total + c)

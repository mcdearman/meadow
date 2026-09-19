-- Three nested loops over flat unboxed arrays of doubles: the shape of
-- numerical code, and of nothing else. `i k j` order, so the innermost loop
-- walks in order.
--
-- `STUArray` rather than a list or a boxed array: a list of boxed doubles is a
-- different program with a different cost, and nobody writing this for real
-- would use one.

import Control.Monad (forM_)
import Control.Monad.ST (ST, runST)
import Data.Array.Base (unsafeRead, unsafeWrite)
import Data.Array.ST (STUArray, newArray)

n :: Int
n = 256

build :: ST s (STUArray s Int Double)
build = newArray (0, n * n - 1) 0

main :: IO ()
main = print (truncate total :: Int)
  where
    total = runST $ do
      a <- build
      b <- build
      c <- build
      forM_ [0 .. n - 1] $ \i -> forM_ [0 .. n - 1] $ \j -> do
        unsafeWrite a (i * n + j) (fromIntegral ((i + j) `mod` 10))
        unsafeWrite b (i * n + j) (fromIntegral ((i * j) `mod` 10))
      forM_ [0 .. n - 1] $ \i -> forM_ [0 .. n - 1] $ \k -> do
        aik <- unsafeRead a (i * n + k)
        forM_ [0 .. n - 1] $ \j -> do
          cij <- unsafeRead c (i * n + j)
          bkj <- unsafeRead b (k * n + j)
          unsafeWrite c (i * n + j) (cij + aik * bkj)
      let diag i acc
            | i >= n = return acc
            | otherwise = do
                x <- unsafeRead c (i * n + i)
                diag (i + 1) (acc + x)
      diag 0 (0 :: Double)

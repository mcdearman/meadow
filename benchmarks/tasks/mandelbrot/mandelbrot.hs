-- Data parallelism: a 2000x2000 grid of independent float work, split between
-- threads by taking every Nth row.
--
-- `forkIO` and an `MVar` per worker: `Control.Concurrent` is what base gives,
-- and the `parallel` package is not installed by default.

import Control.Concurrent (forkIO, getNumCapabilities)
import Control.Concurrent.MVar (newEmptyMVar, putMVar, takeMVar)
import Control.Monad (forM, forM_)

side, limit :: Int
side = 2000
limit = 100

escape :: Double -> Double -> Int -> Double -> Double -> Int
escape cx cy n x y
  | n >= limit = limit
  | x * x + y * y > 4.0 = n
  | otherwise = escape cx cy (n + 1) (x * x - y * y + cx) (2.0 * x * y + cy)

band :: Int -> Int -> Int
band start step = sum [rowAt j | j <- [start, start + step .. side - 1]]
  where
    rowAt j =
      let cy = fromIntegral j / fromIntegral side * 3.0 - 1.5
       in sum [ escape (fromIntegral i / fromIntegral side * 3.0 - 2.0) cy 0 0.0 0.0
              | i <- [0 .. side - 1]
              ]

main :: IO ()
main = do
  workers <- getNumCapabilities
  boxes <- forM [0 .. workers - 1] $ \w -> do
    box <- newEmptyMVar
    _ <- forkIO (putMVar box $! band w workers)
    return box
  totals <- mapM takeMVar boxes
  print (sum totals)

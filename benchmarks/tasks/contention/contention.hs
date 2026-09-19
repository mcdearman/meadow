-- Threads contending for shared mutable state: eight of them moving money
-- between sixteen accounts, every transfer reading two accounts and writing
-- two as one indivisible step.
--
-- `TVar`s and `atomically`: the same answer Meadow gives, from the language the
-- idea came from.

import Control.Concurrent (forkIO, getNumCapabilities)
import Control.Concurrent.MVar (newEmptyMVar, putMVar, takeMVar)
import Control.Concurrent.STM
import Control.Monad (forM, forM_, when)
import Data.List (intercalate)

accounts, workers, moves :: Int
accounts = 16
workers = 8
moves = 20000

transfers :: [TVar Int] -> Int -> Int -> IO ()
transfers bank s0 n
  | n == 0 = return ()
  | otherwise = do
      let s = s0 * 48271 `mod` 2147483647
          a = s `mod` accounts
          b = s `div` accounts `mod` accounts
          amount = 1 + s `mod` 10
      when (a /= b) $ atomically $ do
        modifyTVar' (bank !! a) (subtract amount)
        modifyTVar' (bank !! b) (+ amount)
      transfers bank s (n - 1)

main :: IO ()
main = do
  bank <- mapM newTVarIO (replicate accounts 1000)
  boxes <- forM [0 .. workers - 1] $ \w -> do
    box <- newEmptyMVar
    _ <- forkIO (transfers bank (w + 1) moves >> putMVar box ())
    return box
  mapM_ takeMVar boxes
  finals <- mapM readTVarIO bank
  putStrLn (intercalate "," (map show finals))

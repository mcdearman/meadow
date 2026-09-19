-- Message passing: four producer threads each send fifty thousand numbers into
-- one channel, and the main thread receives all of them and adds them up.

import Control.Concurrent (forkIO)
import Control.Concurrent.Chan
import Control.Monad (forM_)

producers, each :: Int
producers = 4
each = 50000

drain :: Chan Int -> Int -> Int -> IO Int
drain _ 0 acc = return acc
drain ch n acc = do
  v <- readChan ch
  drain ch (n - 1) $! acc + v

main :: IO ()
main = do
  ch <- newChan
  forM_ [0 .. producers - 1] $ \p ->
    forkIO (forM_ [0 .. each - 1] $ \i -> writeChan ch (p * each + i))
  print =<< drain ch (producers * each) 0

-- Strings, a hash map and a sort: count the words of a 3MB file and report the
-- ten commonest, count first and then the word.
--
-- `Data.Map.Strict` over `ByteString`s: `String` is a list of boxed characters,
-- and reading three megabytes into one is a different benchmark.

import qualified Data.ByteString.Char8 as B
import Data.List (sortBy)
import qualified Data.Map.Strict as M
import Data.Ord (comparing)

main :: IO ()
main = do
  text <- B.readFile "work/corpus.txt"
  let counts = M.fromListWith (+) [(w, 1 :: Int) | w <- B.words text]
      ranked = sortBy (comparing (\(w, c) -> (negate c, w))) (M.toList counts)
      top = [B.unpack w ++ ":" ++ show c | (w, c) <- take 10 ranked]
  putStrLn (unwords top)

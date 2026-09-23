-- `wordfreq`, with an arena: the same counts, with nothing allocated per word.
--
-- The corpus is read once into one `ByteString`, and a word is a pair of
-- numbers into it -- where it starts and how long it is -- so no word is ever
-- copied. The table is three unboxed arrays of a fixed size rather than a
-- `Map`, and probing writes numbers into them. What is left in the number is
-- the hashing, the probing and the byte comparisons, with the allocator and
-- the string type taken out.
--
-- Only the ten reported at the end become strings.

import Control.Monad (when)
import Control.Monad.ST (ST, runST)
import Data.Array.Base (unsafeRead, unsafeWrite)
import Data.Array.ST (STUArray, newArray)
import Data.Bits (shiftL, xor, (.&.))
import qualified Data.ByteString.Char8 as B
import Data.List (intercalate)
import Data.Word (Word64, Word8)

cap :: Int
cap = 1 `shiftL` 14 -- 16384 slots for a vocabulary of 5000

data Table s = Table
  { tStarts :: STUArray s Int Int,
    tLens :: STUArray s Int Int,
    tCounts :: STUArray s Int Int
  }

hashOf :: B.ByteString -> Int -> Int -> Word64
hashOf text at n = go 0 1469598103934665603
  where
    go i h
      | i >= n = h
      | otherwise =
          let b = fromIntegral (fromEnum (B.index text (at + i))) :: Word64
           in go (i + 1) ((h `xor` b) * 1099511628211)

same :: B.ByteString -> Int -> Int -> Int -> Bool
same text a b n = go 0
  where
    go i
      | i >= n = True
      | B.index text (a + i) /= B.index text (b + i) = False
      | otherwise = go (i + 1)

bump :: Table s -> B.ByteString -> Int -> Int -> ST s ()
bump t text at n = go (fromIntegral (hashOf text at n) .&. (cap - 1))
  where
    go i = do
      c <- unsafeRead (tCounts t) i
      if c == 0
        then do
          unsafeWrite (tStarts t) i at
          unsafeWrite (tLens t) i n
          unsafeWrite (tCounts t) i 1
        else do
          l <- unsafeRead (tLens t) i
          s <- unsafeRead (tStarts t) i
          if l == n && same text s at n
            then unsafeWrite (tCounts t) i (c + 1)
            else go ((i + 1) .&. (cap - 1))

-- A slot as the three numbers that decide its place: count, start, length.
type Slot = (Int, Int, Int)

before :: B.ByteString -> Slot -> Slot -> Bool
before text (ca, sa, la) (cb, sb, lb)
  | ca /= cb = ca > cb
  | otherwise = B.take la (B.drop sa text) < B.take lb (B.drop sb text)

main :: IO ()
main = do
  text <- B.readFile "work/corpus.txt"
  let slots = runST (count text)
      top = foldl (keep text) [] slots
  putStrLn (intercalate " " [B.unpack (B.take l (B.drop s text)) ++ ":" ++ show c | (c, s, l) <- top])
  where
    count :: B.ByteString -> ST s [Slot]
    count text = do
      t <- Table <$> newArray (0, cap - 1) 0 <*> newArray (0, cap - 1) 0 <*> newArray (0, cap - 1) 0
      let words' i
            | i >= B.length text = pure ()
            | otherwise = do
                let i' = skip i
                    j = end i'
                when (j > i') (bump t text i' (j - i'))
                if j >= B.length text then pure () else words' j
          skip i
            | i < B.length text && (B.index text i == ' ' || B.index text i == '\n') = skip (i + 1)
            | otherwise = i
          end i
            | i < B.length text && B.index text i /= ' ' && B.index text i /= '\n' = end (i + 1)
            | otherwise = i
      words' 0
      let gather i acc
            | i >= cap = pure (reverse acc)
            | otherwise = do
                c <- unsafeRead (tCounts t) i
                if c == 0
                  then gather (i + 1) acc
                  else do
                    s <- unsafeRead (tStarts t) i
                    l <- unsafeRead (tLens t) i
                    gather (i + 1) ((c, s, l) : acc)
      gather 0 []

    -- The ten commonest, kept in order as the table is walked.
    keep text top slot = take 10 (insert top)
      where
        insert [] = [slot]
        insert (x : xs)
          | before text slot x = slot : x : xs
          | otherwise = x : insert xs

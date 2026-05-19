module Utils where

import Data.Text (Text)
import Data.Text.Internal.Search qualified as T
import Data.Vector.Unboxed qualified as U

newtype LineIndex = LineIndex {lineStarts :: U.Vector Int} deriving (Show, Eq)

buildLineIndex :: Text -> LineIndex
buildLineIndex bs = LineIndex . U.fromList $ 0 : map (+ 1) (T.indices "\n" bs)

offsetToLineCol :: LineIndex -> Int -> (Int, Int)
offsetToLineCol (LineIndex starts) !offset =
  let !i = binarySearch starts offset
      !lineStart = starts U.! i
      !col = offset - lineStart + 1
   in (i + 1, col)

binarySearch :: U.Vector Int -> Int -> Int
binarySearch !v !x = go 0 (U.length v - 1)
  where
    go !low !high
      | low > high = high
      | midVal == x = mid
      | midVal < x = go (mid + 1) high
      | otherwise = go low (mid - 1)
      where
        mid = (low + high) `div` 2
        midVal = v U.! mid

data Located a = Located {unLoc :: a, locSpan :: Span}
  deriving (Show, Eq, Ord, Functor)

data Span = Span
  { spanStart :: {-# UNPACK #-} Int,
    spanEnd :: {-# UNPACK #-} Int
  }
  deriving (Show, Eq, Ord)

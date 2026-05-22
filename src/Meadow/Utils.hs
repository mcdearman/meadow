module Meadow.Utils where

import Data.Text (Text)
import Data.Text.Internal.Search qualified as T
import Data.Text.Lazy qualified as TL
import Data.Vector.Unboxed qualified as U
import Prettyprinter
import Text.Pretty.Simple (pShowOpt)
import Text.Pretty.Simple.Internal.Printer

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

instance (Pretty a) => Pretty (Located a) where
  pretty (Located a s) = pretty a <> " @ " <> pretty s

data Span = Span
  { spanStart :: {-# UNPACK #-} Int,
    spanEnd :: {-# UNPACK #-} Int
  }
  deriving (Show, Eq, Ord)

instance Pretty Span where
  pretty (Span s e) = "Span(" <> pretty s <> ", " <> pretty e <> ")"

toPair :: Span -> (Int, Int)
toPair (Span s e) = (s, e)

fromPair :: (Int, Int) -> Span
fromPair (s, e) = Span s e

-- slice :: Span -> ByteString -> ByteString
-- slice (Span s e) bs = BS.take (e - s) (BS.drop s bs)

instance Semigroup Span where
  Span s1 e1 <> Span s2 e2 = Span (min s1 s2) (max e1 e2)

prettyShowNoColor :: (Show a) => a -> String
prettyShowNoColor a = TL.unpack $ pShowOpt config a
  where
    config =
      defaultOutputOptionsDarkBg
        { outputOptionsColorOptions = Nothing
        }
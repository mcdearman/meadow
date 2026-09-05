module Meadow.Layout where

import Meadow.Token (LToken, Token (..))
import Meadow.Utils (LineIndex (..), Located (..), Span (..), offsetToLineCol)

data LayoutError
  = NestedLayoutContextError Int Int Span
  | UnexpectedClosingBraceError Span
  | NotInLayoutContextAtEndOfInputError Span
  deriving (Show, Eq, Ord)

data Marked
  = Token LToken
  | OpenBlock Int (Maybe Token) Span
  | Indent Int Span
  deriving (Show, Eq, Ord)

-- instance Pretty Marked where
--   pretty = \case
--     Token t -> pretty t
--     OpenBlock n h s -> "OpenBlock(" <> pretty n <> ", " <> pretty h <> ") @ " <> pretty s
--     Indent n s -> "Indent(" <> pretty n <> ") @ " <> pretty s

mark :: LineIndex -> [LToken] -> [Marked]
mark li = markIndents . markBlocks
  where
    skipNewlines :: [LToken] -> [LToken]
    skipNewlines (Located TNewline _ : ts) = dropWhile isNewline ts
      where
        isNewline (Located TNewline _) = True
        isNewline _ = False
    skipNewlines ts = ts

    markIndents :: [Marked] -> [Marked]
    markIndents (m@(OpenBlock {}) : ms) = m : markIndents ms
    markIndents [Token n] | unLoc n == TNewline = []
    markIndents (Token n : m@(Token t) : ms)
      | unLoc n == TNewline =
          let (_, col) = offsetToLineCol li (spanStart $ locSpan t)
           in Indent col (locSpan n) : m : markIndents ms
    markIndents (m : ms) = m : markIndents ms
    markIndents [] = []

    markBlocks :: [LToken] -> [Marked]
    markBlocks [t@(hasCloser -> h@(Just _))] = [Token t, OpenBlock 0 h (locSpan t)]
    markBlocks (t@(hasCloser -> h@(Just _)) : n : ts) | unLoc n /= TLBrace = case skipNewlines (n : ts) of
      [] -> [Token t, OpenBlock 0 h (locSpan t)]
      (t' : ts') ->
        let (_, col) = offsetToLineCol li (spanStart $ locSpan t')
         in Token t : OpenBlock col h (locSpan t) : markBlocks (t' : ts')
    markBlocks (t : n : ts)
      | isLayoutHerald (unLoc t) && unLoc n /= TLBrace =
          case skipNewlines (n : ts) of
            [] -> [Token t, OpenBlock 0 Nothing (locSpan t)]
            (t' : ts') ->
              let (_, col) = offsetToLineCol li (spanStart $ locSpan t')
               in Token t : OpenBlock col Nothing (locSpan t) : Token t' : markBlocks ts'
    markBlocks (t : ts) = Token t : markBlocks ts
    markBlocks [] = []

    hasCloser :: LToken -> Maybe Token
    hasCloser = go . unLoc
      where
        go TLet = Just TLet
        go TDo = Just TDo
        go _ = Nothing

    isLayoutHerald :: Token -> Bool
    isLayoutHerald TLet = True
    isLayoutHerald TDo = True
    isLayoutHerald TWhere = True
    isLayoutHerald TOf = True
    isLayoutHerald _ = False

layout :: LineIndex -> [LToken] -> ([LToken], [LayoutError])
layout li ts' =
  let (ts, es) = go (mark li ts') [(1, Nothing)]
   in (Located TVLBrace (Span 0 0) : ts, es)
  where
    go :: [Marked] -> [(Int, Maybe Token)] -> ([LToken], [LayoutError])
    go (i@(Indent n s) : ts) ((m, h) : ms)
      | m == n = let (ts'', es) = go ts ((m, h) : ms) in (Located TVSemi s : ts'', es)
      | n < m = case (ts, h) of
          (Token t : rst, Just h') | closesBlock (unLoc t) h' -> let (ts'', es) = go rst ms in (Located TVRBrace s : t : ts'', es)
          _ -> let (ts'', es) = go (i : ts) ms in (Located TVRBrace s : ts'', es)
    go (Indent {} : ts) ms = go ts ms
    go (OpenBlock n h s : ts) ((m, h') : ms)
      | n > m = let (ts'', es) = go ts ((n, h) : (m, h') : ms) in (Located TVLBrace s : ts'', es)
      | otherwise = let (ts'', es) = go ts ms in (ts'', NestedLayoutContextError m n s : es)
    go (OpenBlock n h s : ts) []
      | n > 0 = let (ts'', es) = go ts [(n, h)] in (Located TVLBrace s : ts'', es)
    go (OpenBlock n _ s : ts) ms = let (ts'', es) = go (Indent n s : ts) ms in (Located TVLBrace s : Located TVRBrace s : ts'', es)
    go (Token t : ts) ((0, Nothing) : ms) | unLoc t == TRBrace = let (ts'', es) = go ts ms in (t : ts'', es)
    go (Token t : Token kw : ts) ((0, Just h) : ms) | unLoc t == TRBrace && closesBlock (unLoc kw) h = let (ts'', es) = go ts ms in (t : kw : ts'', es)
    go (Token t : ts) ms | unLoc t == TRBrace = let (ts'', es) = go ts ms in (ts'', UnexpectedClosingBraceError (locSpan t) : es)
    go (Token kw : Token lb : ts) ms
      | unLoc lb == TLBrace && hasCloser (unLoc kw) =
          let (ts'', es) = go ts ((0, Just (unLoc kw)) : ms) in (kw : lb : ts'', es)
      where
        hasCloser TLet = True
        hasCloser TDo = True
        hasCloser _ = False
    go (Token t : ts) ms | unLoc t == TLBrace = let (ts'', es) = go ts ((0, Nothing) : ms) in (t : ts'', es)
    go (Token t : ts) ((m, Just kw) : ms)
      | m /= 0 && closesBlock (unLoc t) kw = let (ts'', es) = go ts ms in (Located TVRBrace (locSpan t) : t : ts'', es)
    go (Token t : ts) ms = let (ts'', es) = go ts ms in (t : ts'', es)
    go [] [] = ([], [])
    go [] ((m, _) : ms)
      | m /= 0 = let (ts'', es) = go [] ms in (Located TVRBrace (lastSpan ts') : ts'', es)
      | otherwise = ([], [NotInLayoutContextAtEndOfInputError (lastSpan ts')])
      where
        lastSpan [] = Span 0 0
        lastSpan [t] = locSpan t
        lastSpan (_ : rst) = lastSpan rst

    closesBlock :: Token -> Token -> Bool
    closesBlock TIn TLet = True
    closesBlock TWhere TDo = True
    closesBlock _ _ = False

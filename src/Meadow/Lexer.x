{
module Meadow.Lexer (tokenize, nextToken) where

import Meadow.Token
import Data.ByteString.Lazy (ByteString)
import qualified Data.ByteString.Lazy as BS
import qualified Data.Text.Encoding as TE
import qualified Data.Text.Encoding.Error as TEE
import Data.Text (Text, stripPrefix)
import qualified Data.Text as T
import qualified Data.Char as Char
import Meadow.Utils
import Data.Maybe (fromMaybe)
import Control.Monad.State.Strict
import Error.Diagnose 
}

%encoding "utf8"

$unispace   = \x05
$nonzero    = [1-9]
$digit      = [0-9]
$bindig     = [01]
$octdig     = [0-7]
$hexdig     = [0-9A-Fa-f]      
$alpha      = [a-zA-Z]
$lower      = [_a-z]
$upper      = [A-Z]
$nonWhite   = [^$white]
$tab        = \t
$whitespace = [\ $unispace\v]
$newline    = [\n\r\f]
$identChar  = [$alpha $digit \_ \']
$opChar     = [\!\$\%\&\*\+\.\/\<\=\>\?\@\|\\\~\:\^-\`]

$strBare    = [^\"\\\n\xD800-\xDFFF]
$bareScalar = [^\'\\\n\xD800-\xDFFF]
$escSimple  = [0\'\"\\nrtabfv] 

@escByte = x ($hexdig | $hexdig $hexdig)

@u4      = $hexdig $hexdig $hexdig $hexdig
@u8      = @u4 @u4
@escUni4 = u @u4
@escUni8 = U @u8

@lowerCaseIdent = $lower $identChar*
@upperCaseIdent = $upper $identChar*
@opIdent        = $opChar+
@conOpIdent     = ":" $opChar+

@binary      = "0b" $bindig+
@octal       = "0o" $octdig+
@hexadecimal = "0x" $hexdig+
@decimal     = ($nonzero $digit* | "0")
@int         = @binary | @octal | @hexadecimal | @decimal

@esc     = \\ ( $escSimple | @escByte | @escUni4 | @escUni8 )
@char    = \' ( $bareScalar | @esc ) \'
@strChar = ( $strBare | $escSimple | @escByte | @escUni4 | @escUni8 )
@string  = \" @strChar* \"

idyll :-

  $tab                           { tok TTab }
  $whitespace+                   { tok TWhitespace }
  "--".*                         { tok TComment }
  $newline                       { tok TNewline }

  "("                            { tok TLParen }
  ")"                            { tok TRParen }
  "{"                            { tok TLBrace }
  "}"                            { tok TRBrace }
  "["                            { tok TLBracket }
  "]"                            { tok TRBracket }
  "!"                            { tok TBang }
  "#"                            { tok THash }
  [\\]                           { tok TBackSlash }
  ":"                            { tok TColon }
  ";"                            { tok TSemi }
  ","                            { tok TComma }
  "."                            { tok TPeriod }
  "="                            { tok TEq }
  "<-"                           { tok TLArrow }
  "->"                           { tok TRArrow }
  "=>"                           { tok TLFatArrow }
  "|"                            { tok TBar }
  "_"                            { tok TUnderscore }
  "mod"                          { tok TMod }
  "use"                          { tok TUse }
  "data"                         { tok TData }
  "type"                         { tok TType }
  "let"                          { tok TLet }
  "in"                           { tok TIn }
  "where"                        { tok TWhere }
  "do"                           { tok TDo }
  "case"                         { tok TCase }
  "of"                           { tok TOf }
  "if"                           { tok TIf }
  "then"                         { tok TThen }
  "else"                         { tok TElse }

  @lowerCaseIdent                { tok TLowercaseIdent }
  @upperCaseIdent                { tok TUppercaseIdent }
  @conOpIdent                    { tok TConOpIdent }
  @opIdent                       { tok TOpIdent }

  @int                           { tok TInt }
  @char                          { tok TChar }
  @string                        { tok TString }
{


data AlexInput = AlexInput
  { aiOffset :: {-# UNPACK #-} Int   -- ^ 0-based byte offset
  , aiPrev   :: {-# UNPACK #-} Char  -- ^ required by alexInputPrevChar
  , aiBytes  :: ByteString
  }

initInput :: ByteString -> AlexInput
initInput = AlexInput 0 '\n'

alexInputPrevChar :: AlexInput -> Char
alexInputPrevChar = aiPrev

alexGetByte :: AlexInput -> Maybe (Word8, AlexInput)
alexGetByte inp = case BS.uncons (aiBytes inp) of
  Nothing -> Nothing
  Just (w, rest) ->
    Just (w, AlexInput (aiOffset inp + 1) (Char.chr (fromIntegral w)) rest)

-- | Consume one whole UTF-8 scalar. Used only for error recovery.
skipChar :: AlexInput -> Maybe AlexInput
skipChar inp = dropCont . snd <$> alexGetByte inp
  where
    dropCont i = case BS.uncons (aiBytes i) of
      Just (w, _) | isCont w -> maybe i (dropCont . snd) (alexGetByte i)
      _ -> i

isCont :: Word8 -> Bool
isCont w = w .&. 0xC0 == 0x80

-- ---------------------------------------------------------------------------
-- The Lexer monad
-- ---------------------------------------------------------------------------

-- | A diagnostic recorded during scanning. Positions stay as byte offsets;
-- they are resolved to line/column by 'lexerDiagnostic'.
data Diag = Diag
  { diagSpan     :: !Span
  , diagHeadline :: !Text
  , diagLabel    :: !Text
  } deriving (Show, Eq)

data LexerState = LexerState
  { lexInput :: !AlexInput
  , lexDiags :: [Diag]   -- ^ accumulated in reverse
  }

type Lexer = State LexerState

runLexer :: ByteString -> Lexer a -> (a, [Diag])
runLexer src m =
  let (a, st) = runState m (LexerState (initInput src) [])
  in (a, reverse (lexDiags st))

emit :: Span -> Text -> Text -> Lexer ()
emit sp headline label = modify' $ \st ->
  st { lexDiags = Diag sp headline label : lexDiags st }

-- ---------------------------------------------------------------------------
-- Scanning
-- ---------------------------------------------------------------------------

type Action = Span -> ByteString -> Lexer Token

-- | Always terminates: yields 'TEOF' forever once the input is exhausted.
nextToken :: Lexer Token
nextToken = do
  inp <- gets lexInput
  let start = aiOffset inp
  case alexScan inp 0 of
    AlexEOF -> pure (Located TEOF (Span start start))
    AlexSkip inp' _ -> do
      modify' $ \st -> st { lexInput = inp' }
      nextToken
    AlexToken inp' len act -> do
      modify' $ \st -> st { lexInput = inp' }
      act (Span start (aiOffset inp')) (BS.take len (aiBytes inp))
    AlexError _ -> case skipChar inp of
      Nothing -> pure (Located TEOF (Span start start))
      Just _ -> do
        let end = recover inp
            sp  = Span start (aiOffset end)
        modify' $ \st -> st { lexInput = end }
        emit sp "unexpected character" "this is not a valid token"
        pure (Located TError sp)

-- | Swallow a maximal run of junk so one bad region yields one diagnostic.
recover :: AlexInput -> AlexInput
recover i = case skipChar i of
  Nothing -> i
  Just i' -> case alexScan i' 0 of
    AlexError _ -> recover i'
    _           -> i'

tokenize :: ByteString -> ([Token], [Diag])
tokenize src = runLexer src (go [])
  where
    go acc = do
      t <- nextToken
      case unLoc t of
        TEOF -> pure (reverse (t : acc))
        _    -> go (t : acc)

-- ---------------------------------------------------------------------------
-- Actions
-- ---------------------------------------------------------------------------

tok :: T -> Action
tok t sp _ = pure (Located t sp)

tokWith :: (Text -> T) -> Action
tokWith f sp bs = pure (Located (f (bsToText bs)) sp)

errTok :: Span -> Token
errTok = Located TError

intToken :: Action
intToken sp bs = pure (Located (TInt (parseRadix radix digits)) sp)
  where
    t = bsToText bs
    (radix, digits)
      | T.length t > 1, T.head t == '0' = case Char.toLower (T.index t 1) of
          'b' -> (2,  T.drop 2 t)
          'o' -> (8,  T.drop 2 t)
          'x' -> (16, T.drop 2 t)
          _   -> (10, t)
      | otherwise = (10, t)

badIntToken :: Action
badIntToken sp bs = do
  emit sp ("malformed numeric literal: " <> bsToText bs)
          "expected only digits valid for this base"
  pure (errTok sp)

unterminated :: Text -> Action
unterminated what sp _ = do
  emit sp ("unterminated " <> what <> " literal")
          "no closing quote before the end of the line"
  pure (errTok sp)

stringToken :: Action
stringToken sp bs = unescapeBody sp (literalBody bs) >>= \case
  Nothing  -> pure (errTok sp)
  Just txt -> pure (Located (TString txt) sp)

charToken :: Action
charToken sp bs = unescapeBody sp (literalBody bs) >>= \case
  Nothing -> pure (errTok sp)
  Just txt -> case T.unpack txt of
    [c] -> pure (Located (TChar c) sp)
    []  -> do emit sp "empty character literal" "expected exactly one character"
              pure (errTok sp)
    _   -> do emit sp "character literal has more than one character"
                      "expected exactly one character"
              pure (errTok sp)

literalBody :: ByteString -> Text
literalBody = T.dropEnd 1 . T.drop 1 . bsToText

-- | Decode escapes, reporting each bad one at its own span.
-- Returns 'Nothing' if anything was wrong, so callers don't double-report.
unescapeBody :: Span -> Text -> Lexer (Maybe Text)
unescapeBody sp body = do
  let (txt, errs) = unescape body
      open = spanStart sp + 1   -- skip the opening quote
  forM_ errs $ \(EscError off len msg) ->
    emit (Span (open + off) (open + off + len)) msg "invalid escape sequence"
  pure (if null errs then Just txt else Nothing)

-- ---------------------------------------------------------------------------
-- Escape decoding
-- ---------------------------------------------------------------------------

-- | Byte offset within the literal body, byte length, message.
data EscError = EscError !Int !Int !Text

unescape :: Text -> (Text, [EscError])
unescape input = case go 0 input [] [] of
  (cs, es) -> (T.pack (reverse cs), reverse es)
  where
    go !i t acc errs = case T.uncons t of
      Nothing           -> (acc, errs)
      Just ('\\', rest) -> esc i rest acc errs
      Just (c, rest)    -> go (i + utf8Len c) rest (c : acc) errs

    esc !i t acc errs = case T.uncons t of
      Nothing -> (acc, EscError i 1 "incomplete escape sequence" : errs)
      Just (c, rest)
        | Just r <- simple c -> go (i + 2) rest (r : acc) errs
        | c == 'x'  -> num i 1 2 rest acc errs
        | c == 'u'  -> num i 4 4 rest acc errs
        | c == 'U'  -> num i 8 8 rest acc errs
        | otherwise -> go (i + 1 + utf8Len c) rest acc
            (EscError i (1 + utf8Len c)
               ("unknown escape sequence: \\" <> T.singleton c) : errs)

    num !i lo hi t acc errs =
      let ds   = T.takeWhile Char.isHexDigit (T.take hi t)
          n    = T.length ds
          rest = T.drop n t
          len  = 2 + n      -- escapes are ASCII, so chars == bytes
          val  = parseRadix 16 ds
      in if n < lo
           then go (i + len) rest acc
                  (EscError i len "escape sequence needs more hexadecimal digits" : errs)
         else if val > 0x10FFFF || (val >= 0xD800 && val <= 0xDFFF)
           then go (i + len) rest acc
                  (EscError i len "not a valid Unicode scalar value" : errs)
           else go (i + len) rest (Char.chr (fromIntegral val) : acc) errs

    simple = \case
      '0'  -> Just '\0'
      'n'  -> Just '\n'
      'r'  -> Just '\r'
      't'  -> Just '\t'
      'a'  -> Just '\a'
      'b'  -> Just '\b'
      'f'  -> Just '\f'
      'v'  -> Just '\v'
      '\\' -> Just '\\'
      '\'' -> Just '\''
      '"'  -> Just '"'
      _    -> Nothing

-- ---------------------------------------------------------------------------
-- Offsets -> line/column, for rendering
-- ---------------------------------------------------------------------------

data SrcIndex = SrcIndex
  { siSrc    :: !ByteString
  , siStarts :: !(UArray Int Int)   -- ^ byte offset of each line's first byte
  }

mkSrcIndex :: ByteString -> SrcIndex
mkSrcIndex src = SrcIndex src (listArray (0, length ss - 1) ss)
  where
    ss = lineStarts src

lineStarts :: ByteString -> [Int]
lineStarts src = 0 : go 0
  where
    n = BS.length src
    go !i
      | i >= n = []
      | otherwise = case BS.index src i of
          0x0A -> (i + 1) : go (i + 1)
          0x0D | i + 1 < n, BS.index src (i + 1) == 0x0A -> go (i + 1)
               | otherwise -> (i + 1) : go (i + 1)
          _ -> go (i + 1)

-- | Bundle diagnostics with the source so 'printDiagnostic' renders snippets.
lexerDiagnostic :: FilePath -> ByteString -> [Diag] -> Diagnostic Text
lexerDiagnostic path src = foldl addReport withFile . map toReport
  where
    txt      = bsToText src
    idx      = buildLineIndex txt
    withFile = addFile mempty path (T.unpack txt)

    toReport (Diag sp msg) = Err Nothing msg [(pos sp, This msg)] []
    pos (Span s e) = Position (offsetToLineCol idx s) (offsetToLineCol idx e) path

-- ---------------------------------------------------------------------------
-- Small helpers
-- ---------------------------------------------------------------------------

parseRadix :: Integer -> Text -> Integer
parseRadix r = T.foldl' (\acc c -> acc * r + fromIntegral (Char.digitToInt c)) 0

utf8Len :: Char -> Int
utf8Len c
  | n < 0x80    = 1
  | n < 0x800   = 2
  | n < 0x10000 = 3
  | otherwise   = 4
  where n = Char.ord c

-- Lenient: a lexer that reports errors shouldn't throw on bad UTF-8.
bsToText :: ByteString -> Text
bsToText = TE.decodeUtf8With TEE.lenientDecode
}
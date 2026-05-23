module Meadow.Lexer where

import Control.Applicative (empty, liftA, optional, (<|>))
import Control.Monad (void)
import Data.Data (Proxy (..))
import Data.List.NonEmpty qualified as NE
import Data.Text (Text, pack, unpack)
import Data.Text qualified as T
import Data.Void (Void)
import Error.Diagnose.Compat.Megaparsec
import Meadow.Token
import Meadow.Utils
import Text.Megaparsec
  ( MonadParsec (eof, getParserState, lookAhead, notFollowedBy, takeWhile1P, takeWhileP, token, try),
    ParseErrorBundle,
    Parsec,
    PosState (..),
    TraversableStream (..),
    choice,
    getOffset,
    many,
    manyTill,
    oneOf,
    option,
    parse,
    some,
    (<?>),
  )
import Text.Megaparsec.Char (alphaNumChar, char, char', hspace1, lowerChar, space1, string, upperChar)
import Text.Megaparsec.Char.Lexer qualified as L
import Text.Megaparsec.Stream hiding (Token)

type Lexer = Parsec Void Text

tokenize :: Text -> Either (ParseErrorBundle Text Void) [LToken]
tokenize = parse (sc *> some (located tokenL <* sc)) ""

tokenL :: Lexer Token
tokenL =
  choice
    [ TokenMod <$ string "mod",
      TokenData <$ string "data",
      TokenType <$ string "type",
      TokenDef <$ string "def",
      TokenFun <$ string "fun",
      TokenLet <$ string "let",
      TokenRec <$ string "rec",
      TokenIn <$ string "in",
      TokenMatch <$ string "match",
      TokenWith <$ string "with",
      TokenIf <$ string "if",
      TokenThen <$ string "then",
      TokenElse <$ string "else",
      upperCaseIdent,
      lowerCaseIdent,
      conOpIdent,
      opIdent,
      int,
      str,
      charT,
      TokenLParen <$ char '(',
      TokenRParen <$ char ')',
      TokenLBrace <$ char '{',
      TokenRBrace <$ char '}',
      TokenLBracket <$ char '[',
      TokenRBracket <$ char ']',
      TokenBang <$ char '!',
      TokenRArrow <$ string "->",
      TokenBackSlash <$ char '\\',
      TokenColon <$ char ':',
      TokenSemi <$ char ';',
      TokenComma <$ char ',',
      TokenPeriod <$ char '.',
      TokenRFatArrow <$ string "=>",
      TokenEq <$ char '=',
      TokenLArrow <$ string "<-",
      TokenBar <$ char '|',
      TokenUnderscore <$ char '_'
    ]

lowerCaseIdent :: Lexer Token
lowerCaseIdent = try $ do
  name <- pack <$> ((:) <$> identStartChar <*> many identChar)
  pure $ TokenLowercaseIdent name
  where
    identStartChar = lowerChar <|> char '_'
    identChar = alphaNumChar <|> char '_' <|> char '\''

upperCaseIdent :: Lexer Token
upperCaseIdent = TokenUppercaseIdent . pack <$> ((:) <$> upperChar <*> many alphaNumChar)

opIdent :: Lexer Token
opIdent = try $ do
  sym <- choice [startSpecial, startNotEq] <* notFollowedBy (oneOf ['=', '.', '@', '|', ':', '-'])
  if sym `elem` reservedSymbols
    then fail $ "symbol " ++ unpack sym ++ " cannot be used in place of identifier"
    else pure $ TokenOpIdent sym
  where
    opStartChar = oneOf ("!$%&*+/<>?~" ++ ['^' .. '`'] :: String)
    startSpecial = try $ T.cons <$> oneOf ['=', '.', '@', '|', ':'] <*> takeWhile1P Nothing isOpChar
    startNotEq = T.cons <$> opStartChar <*> takeWhileP Nothing isOpChar

    reservedSymbols :: [Text]
    reservedSymbols =
      [ "->",
        "=>",
        "<-",
        "!"
      ]

conOpIdent :: Lexer Token
conOpIdent = try $ TokenConOpIdent <$> (T.cons <$> char ':' <*> takeWhile1P Nothing isOpChar)

isOpChar :: Char -> Bool
isOpChar c = c `elem` ("!$%&*+./<=>?@|\\~:" ++ ['^' .. '`'] :: String)

-- newline :: Lexer Token
-- newline = TokenNewline <$ oneOf ['\n', '\r']

-- whitespace :: Lexer Token
-- whitespace = TokenWhitespace <$ takeWhile1P (Just "whitespace") isSpace
--   where
--     isSpace c = c == ' ' || c == '\t'

lineComment :: Lexer ()
lineComment = L.skipLineComment "--"

binary :: Lexer Integer
binary = try $ char '0' *> char' 'b' *> L.binary

octal :: Lexer Integer
octal = try $ char '0' *> char' 'o' *> L.octal

hexadecimal :: Lexer Integer
hexadecimal = try $ char '0' *> char' 'x' *> L.hexadecimal

int :: Lexer Token
int = TokenInt <$> choice [octal, hexadecimal, binary, L.decimal]

str :: Lexer Token
str = TokenString <$> (char '\"' *> (pack <$> manyTill L.charLiteral (char '\"')))

charT :: Lexer Token
charT = TokenChar <$> (char '\'' *> L.charLiteral <* char '\'')

sc :: Lexer ()
sc = L.space space1 lineComment (L.skipBlockCommentNested "{-" "-}")

located :: Lexer a -> Lexer (Located a)
located l = do
  start <- getOffset
  res <- l
  Located res . Span start <$> getOffset

-- instance VisualStream [LToken] where
--   -- showTokens Proxy = show
--   showTokens _ = unwords . map show . NE.toList

-- instance TraversableStream [LToken] where
--   reachOffset o PosState {..} =
--     ( Just (prefix ++ restOfLine),
--       PosState
--         { pstateInput = post,
--           pstateOffset = max pstateOffset o,
--           pstateSourcePos = newSourcePos,
--           pstateTabWidth = pstateTabWidth,
--           pstateLinePrefix = prefix
--         }
--     )
--     where
--       prefix =
--         if sameLine
--           then pstateLinePrefix ++ preLine
--           else preLine
--       preLine = reverse . takeWhile (/= '\n') . reverse $ preStr
--       (preStr, postStr) = splitAt tokensConsumed (unpack $ streamSrc pstateInput)
--       newSourcePos =
--         case post of
--           [] -> case pstateInput of
--             [] -> pstateSourcePos
--             ts -> let (l, c) = offsetToLineCol $ spanEnd (last ts) in SourcePos (sourceName pstateSourcePos) l c
--           (x : _) -> wpStart x
--       sameLine = sourceLine newSourcePos == sourceLine pstateSourcePos
--       (pre, post) = splitAt (o - pstateOffset) pstateInput
--       tokensConsumed =
--         case NE.nonEmpty pre of
--           Nothing -> 0
--           Just nePre -> tokensLength pxy nePre
--       restOfLine = takeWhile (/= '\n') postStr
--       pxy :: Proxy [LToken]
--       pxy = Proxy

instance HasHints Void msg where
  hints _ = mempty
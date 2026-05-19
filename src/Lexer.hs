module Lexer where

import Control.Applicative (empty, liftA, optional, (<|>))
import Control.Monad (void)
import Data.List.NonEmpty qualified as NE
import Data.Text (Text, pack, unpack)
import Data.Text qualified as T
import Data.Void (Void)
import Error.Diagnose.Compat.Megaparsec
import Meadow.Token (LToken, Token (..))
import Meadow.Utils
import Text.Megaparsec
  ( MonadParsec (eof, getParserState, lookAhead, notFollowedBy, takeWhile1P, takeWhileP, token, try),
    ParseErrorBundle,
    Parsec,
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
import Text.Megaparsec.Stream (VisualStream (..))

type Lexer = Parsec Void Text

tokenize :: Text -> Either (ParseErrorBundle Text Void) [LToken]
tokenize = parse (sc *> some (located tokenL <* sc)) ""

tokenL :: Lexer Token
tokenL =
  choice
    [ newline,
      TokenMod <$ string "mod",
      TokenData <$ string "data",
      TokenType <$ string "type",
      TokenFun <$ string "fun",
      TokenDef <$ string "def",
      TokenLet <$ string "let",
      TokenIn <$ string "in",
      TokenCase <$ string "case",
      TokenOf <$ string "of",
      TokenIf <$ string "if",
      TokenThen <$ string "then",
      TokenElse <$ string "else",
      TokenPlus <$ char '+',
      TokenMinus <$ char '-',
      TokenStar <$ char '*',
      TokenSlash <$ char '/',
      TokenPercent <$ char '%',
      TokenCaret <$ char '^',
      upperCaseIdent,
      lowerCaseIdent,
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
      TokenPipe <$ char '|',
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

isOpChar :: Char -> Bool
isOpChar c = c `elem` ("!$%&*+./<=>?@|\\~:" ++ ['^' .. '`'] :: String)

newline :: Lexer Token
newline = TokenNewline <$ oneOf ['\n', '\r']

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
sc = L.space hspace1 lineComment (L.skipBlockCommentNested "{-" "-}")

located :: Lexer a -> Lexer (Located a)
located l = do
  start <- getOffset
  res <- l
  Located res . Span start <$> getOffset
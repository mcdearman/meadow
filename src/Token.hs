module Token where

import Data.Text (Text)
import Meadow.Utils (Located)

type LToken = Located Token

data Token
  = TokenError
  | TokenNewline
  | TokenUppercaseIdent Text
  | TokenLowercaseIdent Text
  | TokenInt Integer
  | TokenString Text
  | TokenChar Char
  | TokenPlus
  | TokenMinus
  | TokenStar
  | TokenSlash
  | TokenPercent
  | TokenCaret
  | TokenAmpersand
  | TokenLParen
  | TokenRParen
  | TokenLBrace
  | TokenRBrace
  | TokenLBracket
  | TokenRBracket
  | TokenVLBrace
  | TokenVRBrace
  | TokenVSemi
  | TokenBang
  | TokenHash
  | TokenBackSlash
  | TokenColon
  | TokenSemi
  | TokenComma
  | TokenPeriod
  | TokenEq
  | TokenLArrow
  | TokenRArrow
  | TokenRFatArrow
  | TokenPipe
  | TokenUnderscore
  | TokenAt
  | TokenMod
  | TokenData
  | TokenType
  | TokenFun
  | TokenDef
  | TokenLet
  | TokenIn
  | TokenCase
  | TokenOf
  | TokenIf
  | TokenThen
  | TokenElse
  deriving (Show, Eq, Ord)
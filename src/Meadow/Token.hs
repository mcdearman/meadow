module Meadow.Token where

import Data.Text (Text)
import Meadow.Utils

type LToken = Located Token

data Token
  = TokenError
  | TokenUppercaseIdent Text
  | TokenLowercaseIdent Text
  | TokenOpIdent Text
  | TokenConOpIdent Text
  | TokenInt Integer
  | TokenString Text
  | TokenChar Char
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
  | TokenBar
  | TokenUnderscore
  | TokenAt
  | TokenMod
  | TokenUse
  | TokenData
  | TokenType
  | TokenDef
  | TokenFun
  | TokenLet
  | TokenIn
  | TokenMatch
  | TokenWith
  | TokenIf
  | TokenThen
  | TokenElse
  deriving (Show, Eq, Ord)
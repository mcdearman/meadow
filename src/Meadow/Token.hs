module Meadow.Token where

import Data.ByteString (ByteString)
import Meadow.Utils

type LToken = Located Token

data Token
  = TError
  | TNewline
  | TUppercaseIdent ByteString
  | TLowercaseIdent ByteString
  | TOpIdent ByteString
  | TConOpIdent ByteString
  | TInt Integer
  | TString ByteString
  | TChar Char
  | TLParen
  | TRParen
  | TLBrace
  | TRBrace
  | TLBracket
  | TRBracket
  | TVLBrace
  | TVRBrace
  | TVSemi
  | TBang
  | THash
  | TBackSlash
  | TColon
  | TSemi
  | TComma
  | TPeriod
  | TEq
  | TLArrow
  | TRArrow
  | TRFatArrow
  | TBar
  | TUnderscore
  | TAt
  | TMod
  | TUse
  | TData
  | TType
  | TLet
  | TIn
  | TWhere
  | TDo
  | TCase
  | TOf
  | TIf
  | TThen
  | TElse
  deriving (Show, Eq, Ord)
module Meadow.Syn where

import Data.Text (Text)
import Meadow.Utils (Located)

newtype Module
  = Module [LDecl]
  deriving (Show, Eq, Ord)

type LDecl = Located Decl

data Decl
  = DeclBind Bind
  | DeclError
  deriving (Show, Eq, Ord)

type LExpr = Located Expr

data Expr
  = ExprLit Lit
  | ExprVar Ident
  | ExprLam [LPat] LExpr
  | ExprApp LExpr [LExpr]
  | ExprLet [Bind] LExpr
  | ExprIf LExpr LExpr LExpr
  | ExprCase LExpr [(LPat, LExpr)]
  | ExprUnOp LUnOp LExpr
  | ExprBinOp LBinOp LExpr LExpr
  | ExprTuple [LExpr]
  | ExprList [LExpr]
  | ExprCons Ident [LExpr]
  | ExprError
  deriving (Show, Eq, Ord)

type LUnOp = Located UnOp

data UnOp
  = UnNeg
  | UnNot
  deriving (Show, Eq, Ord)

type LBinOp = Located BinOp

data BinOp
  = BinAdd
  | BinSub
  | BinMul
  | BinDiv
  | BinMod
  | BinPow
  | BinEq
  | BinNeq
  | BinLt
  | BinGt
  | BinLeq
  | BinGeq
  deriving (Show, Eq, Ord)

data Bind
  = BindPat LPat
  | BindFun Ident [LPat] LExpr
  deriving (Show, Eq, Ord)

type LPat = Located Pat

data Pat
  = PatWildcard
  | PatLit Lit
  | PatVar Ident
  | PatAs Ident LPat
  | PatCons Ident [LPat]
  | PatTuple [LPat]
  | PatList [LPat]
  | PatError
  deriving (Show, Eq, Ord)

type Ident = Located Text

data Lit
  = LitUnit
  | LitInt Int
  | LitFloat Float
  | LitChar Char
  | LitString Text
  | LitError
  deriving (Show, Eq, Ord)
module Meadow.AST where

import Data.Text (Text)
import Meadow.Utils

type LExpr = Located Expr

data Module = Module {moduleName :: Text, moduleDecls :: [LDecl]}
  deriving (Show, Eq)

type LDecl = Located Decl

data Decl
  = DeclBind Bind
  deriving (Show, Eq)

data Expr
  = ExprVar Ident
  | ExprLit Lit
  | ExprApp LExpr LExpr
  | ExprLam LPat LExpr
  | ExprLet Bind LExpr
  | ExprIf LExpr LExpr LExpr
  | ExprMatch LExpr [(LPat, LExpr)]
  | ExprTuple [LExpr]
  | ExprList [LExpr]
  | ExprUnit
  deriving (Show, Eq)

data Bind = BindPat LPat LExpr | BindFun Ident [LPat] LExpr
  deriving (Show, Eq)

type LPat = Located Pat

data Pat
  = PatWildcard
  | PatVar Ident
  | PatLit Lit
  | PatAs Ident LPat
  | PatCons Ident [LPat]
  | PatTuple [LPat]
  | PatList [LPat]
  | PatUnit
  deriving (Show, Eq)

type Ident = Located Text

data Lit
  = LitInt Integer
  | LitBool Bool
  | LitString Text
  deriving (Show, Eq)
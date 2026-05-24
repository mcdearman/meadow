module Meadow.Parser where

import Control.Monad (void)
import Control.Monad.Combinators.Expr
import Data.Functor (($>))
import Data.List (unsnoc)
import Data.List.NonEmpty qualified as NE
import Data.List.NonEmpty qualified as NonEmpty
import Data.Maybe (fromMaybe, isJust)
import Data.Proxy
import Data.Set qualified as Set
import Data.Text (Text, pack, unpack)
import Data.Void
import Error.Diagnose.Compat.Megaparsec (HasHints (..))
import Meadow.AST
import Meadow.Token
import Meadow.Utils
import Text.Megaparsec hiding (Token)

-- instance HasHints Void msg where
--   hints _ = mempty

type Parser = Parsec Void [LToken]

data ParseResult
  = ParseInteractiveDecl LDecl
  | ParseInteractiveExpr LExpr
  | ParseFile Module
  deriving (Show, Eq)

parseMeadow :: InputMode -> [LToken] -> Either (ParseErrorBundle [LToken] Void) ParseResult
parseMeadow inputMode ts =
  case inputMode of
    InputModeFile filename -> ParseFile <$> parseFile filename ts
    InputModeInteractive -> case parseInteractive ts of
      Left err -> Left err
      Right (Left d) -> Right $ ParseInteractiveDecl d
      Right (Right e) -> Right $ ParseInteractiveExpr e

parseFile :: Text -> [LToken] -> Either (ParseErrorBundle [LToken] Void) Module
parseFile filename = parse (mod' filename) (unpack filename)

parseInteractive :: [LToken] -> Either (ParseErrorBundle [LToken] Void) (Either LDecl LExpr)
parseInteractive = parse (interactive <* eof) "<interactive>"

interactive :: Parser (Either LDecl LExpr)
interactive = Left <$> decl <|> Right <$> expr

mod' :: Text -> Parser Module
mod' name = Module name <$> many decl

decl :: Parser LDecl
decl = located (try funBind <|> patBind)
  where
    funBind =
      try $
        DeclBind
          <$> (tokenP TokenFun *> (BindFun <$> lowerIdent <*> many pat <*> (tokenP TokenEq *> expr)))
    patBind =
      try $
        DeclBind
          <$> (tokenP TokenDef *> (BindPat <$> pat <*> (tokenP TokenEq *> expr)))

expr :: Parser LExpr
expr = app <|> atom
  where
    unit = tokenP TokenLParen *> tokenP TokenRParen $> ExprUnit
    litExpr = ExprLit <$> lit
    varExpr = ExprVar <$> lowerIdent
    lam = ExprLam <$> (tokenP TokenBackSlash *> pat) <*> (tokenP TokenRArrow *> expr)
    let' = ExprLet <$> (tokenP TokenLet *> (BindPat <$> pat <*> (tokenP TokenEq *> expr))) <*> (tokenP TokenIn *> expr)
    letRec = ExprLet <$> (tokenP TokenLet *> tokenP TokenRec *> (BindFun <$> lowerIdent <*> many pat <*> (tokenP TokenEq *> expr))) <*> (tokenP TokenIn *> expr)
    tuple = ExprTuple <$> parens (expr `sepBy` tokenP TokenComma)
    atom = located $ choice [try tuple, try $ parens (unLoc <$> expr), unit, litExpr, try letRec, let', varExpr, lam]
    app = try $ located $ ExprApp <$> atom <*> atom

pat :: Parser LPat
pat = located $ choice [try patWildcard, patLit, patIdent, patCons, patAs, patList, patTuple, patUnit]
  where
    patWildcard = tokenP TokenUnderscore $> PatWildcard
    patLit = PatLit <$> lit
    patIdent = PatVar <$> lowerIdent
    patCons = PatCons <$> upperIdent <*> many pat
    patAs = PatAs <$> lowerIdent <*> (tokenP TokenAt *> pat)
    patList = PatList <$> brackets (pat `sepBy` tokenP TokenComma)
    patTuple = PatTuple <$> parens (pat `sepBy` tokenP TokenComma)
    patUnit = tokenP TokenLParen *> tokenP TokenRParen $> PatUnit

lowerIdent :: Parser Ident
lowerIdent = token (\case (Located (TokenLowercaseIdent name) s) -> Just (Located name s); _ -> Nothing) Set.empty

upperIdent :: Parser Ident
upperIdent = token (\case (Located (TokenUppercaseIdent name) s) -> Just (Located name s); _ -> Nothing) Set.empty

parens :: Parser a -> Parser a
parens = between (tokenP TokenLParen) (tokenP TokenRParen)

brackets :: Parser a -> Parser a
brackets = between (tokenP TokenLBracket) (tokenP TokenRBracket)

braces :: Parser a -> Parser a
braces = between (tokenP TokenLBrace) (tokenP TokenRBrace)

vbraces :: Parser a -> Parser a
vbraces = between (tokenP TokenVLBrace) (tokenP TokenVRBrace)

lit :: Parser Lit
lit =
  choice
    [ LitInt <$> int,
      LitBool <$> bool,
      LitString <$> string
    ]

int :: Parser Integer
int = token (\case (Located (TokenInt n) _) -> Just n; _ -> Nothing) Set.empty

bool :: Parser Bool
bool = token test Set.empty
  where
    test (Located (TokenUppercaseIdent name) _) =
      case name of
        "True" -> Just True
        "False" -> Just False
        _ -> Nothing
    test _ = Nothing

string :: Parser Text
string = token (\case (Located (TokenString str) _) -> Just str; _ -> Nothing) Set.empty

tokenP :: Token -> Parser Token
tokenP t = token (\(Located lt _) -> if t == lt then Just t else Nothing) Set.empty

located :: Parser a -> Parser (Located a)
located p = do
  startInp <- getInput
  x <- p
  endInp <- getInput
  case (startInp, endInp) of
    ([], _) -> fail "empty input"
    (t : _, t' : _) -> pure $ Located x (locSpan t <> locSpan t')
    (t : ts, []) -> case unsnoc ts of
      Nothing -> pure $ Located x (locSpan t)
      Just (_, eoi) -> pure $ Located x (locSpan t <> locSpan eoi)
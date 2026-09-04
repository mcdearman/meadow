{
module Meadow.Lexer (tokenize, nextToken) where

import Meadow.Token
import Data.ByteString.Lazy (ByteString)
import qualified Data.ByteString.Lazy as BL
import qualified Data.Text.Encoding as TE
import Data.Text (Text, stripPrefix)
import qualified Data.Text as T
import qualified Data.Char as Char
import Meadow.Utils
import Data.Maybe (fromMaybe)
import Control.Monad.State.Strict
import Error.Diagnose 
}

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

  @lowerCaseIdent                { tok TLowercaseIdent }
  @upperCaseIdent                { tok TUppercaseIdent }
  @conOpIdent                    { tok TConOpIdent }
  @opIdent                       { tok TOpIdent }

  @int                           { tok TInt }
  @char                          { tok TChar }
  @string                        { tok TString }
{
data AlexInput = AlexInput
  { aiOffset :: !Int          -- byte offset from start of file, 0-based
  , aiPrev   :: !Char         -- last consumed byte, required by alexInputPrevChar
  , aiBytes  :: ByteString    -- remaining input
  }

initInput :: ByteString -> AlexInput
initInput bs = AlexInput 0 '\n' bs

alexGetByte :: AlexInput -> Maybe (Word8, AlexInput)
alexGetByte inp = case BL.uncons (aiBytes inp) of
  Nothing -> Nothing
  Just (w, rest) ->
    Just (w, inp { aiOffset = aiOffset inp + 1
                 , aiPrev   = Char.chr (fromIntegral w)
                 , aiBytes  = rest })

alexInputPrevChar :: AlexInput -> Char
alexInputPrevChar = aiPrev

-- Helper to log an error into the state
logDiagnostic :: AlexPosn -> String -> Lexer ()
logDiagnostic (AlexPn _ line col) msg = modify $ \st ->
  let -- Construct your 'diagnose' report here 
      -- (Adjust this to match your exact 'diagnose' configuration/markers)
      newReport = Err Nothing ("Lexical Error: " ++ msg) [(Position line col line (col + 1) "src", Where msg)] []
  in st { lexerErrors = newReport : lexerErrors st }

tok :: T -> AlexPosn -> ByteString -> Token
tok kind p bs = Token kind (makeSpan p bs)

makeInt :: Int -> ByteString -> T
makeInt 10 bs = TInt $ parseRadix 10 $ bsToText bs
makeInt 2 bs = TInt $ parseRadix 2 $ stripIntPrefix bs
makeInt 8 bs = TInt $ parseRadix 8 $ stripIntPrefix bs
makeInt 16 bs = TInt $ parseRadix 16 $ stripIntPrefix bs
makeInt r _ = error $ "Unsupported radix" ++ show r

stripIntPrefix :: ByteString -> Text
stripIntPrefix bs = T.drop 2 $ bsToText bs

bsToText :: ByteString -> Text
bsToText = TE.decodeUtf8 . BL.toStrict

bToChar :: ByteString -> Char
bToChar = T.head . stripCharQuotes
  where
    stripCharQuotes :: ByteString -> Text
    stripCharQuotes = fromMaybe (error "Invalid char literal") . stripPrefix "'" . T.init . bsToText

parseRadix :: (Integral a) => a -> Text -> a
parseRadix r = T.foldl' step 0
  where
    step a c = a * r + (fromIntegral $ Char.digitToInt c)

makeSpan :: AlexPosn -> ByteString -> Span
makeSpan (AlexPn start _ _) bs = Span start end
  where 
    end = start + (fromIntegral $ BL.length bs)

posnOffset :: AlexPosn -> Int
posnOffset (AlexPn o _ _) = o

tokenize :: ByteString -> [Token]
tokenize = alexScanTokens
}
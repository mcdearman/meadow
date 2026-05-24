module Meadow.Pipeline where

import Data.Text
import Error.Diagnose (addFile, defaultStyle, printDiagnostic, stderr)
import Error.Diagnose.Compat.Megaparsec
import Meadow.Lexer (tokenize)
import Meadow.Parser
import Meadow.Utils
import Text.Megaparsec (errorBundlePretty)
import Text.Pretty.Simple (pPrint)

data PipelineEnv = PipelineEnv
  { pipelineInputMode :: InputMode,
    pipelineLineIndex :: LineIndex
  }

newPipelineEnv :: Text -> InputMode -> PipelineEnv
newPipelineEnv src inputMode = PipelineEnv inputMode (buildLineIndex src)

runPipeline :: PipelineEnv -> Text -> IO ()
runPipeline env src =
  case tokenize src of
    Left errs -> print $ errorBundlePretty errs
    Right tokens -> case parseMeadow (pipelineInputMode env) tokens of
      Left errs ->
        let diag = diagnosticFromBundle (const True) Nothing "Parse error on input" Nothing errs
            diag' = addFile diag "interactive" (unpack src)
         in print $ printDiagnostic stderr True True 2 defaultStyle diag'
      Right res -> pPrint res

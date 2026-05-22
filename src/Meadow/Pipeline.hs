module Meadow.Pipeline where

import Data.Text
import Meadow.Lexer (tokenize)
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
    Left err -> print $ errorBundlePretty err
    Right tokens -> pPrint tokens

data InputMode
  = InputModeFile Text
  | InputModeInteractive
  deriving (Show, Eq)

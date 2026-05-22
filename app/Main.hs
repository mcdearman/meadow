module Main where

import Data.Text (Text)
import Meadow.Pipeline
import Meadow.Utils

src :: Text
src =
  """
  fun map (f, xs) =
    match xs with
    | [] -> []
    | x :: xs -> f x :: map (f, xs)
  """

main :: IO ()
main = runPipeline (newPipelineEnv src InputModeInteractive) src
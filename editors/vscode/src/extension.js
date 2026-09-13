// The VS Code client: it starts `meadow lsp` and lets the server do the work.
//
// Everything the extension contributes beyond this file is declarative — the
// language id, the TextMate grammar and the indentation rules in
// `package.json` / `language-configuration.json`. Those apply the moment a
// `.mw` file opens, whether or not the server is running, which is why a
// missing `meadow` degrades to plain syntax highlighting rather than a broken
// editor.

const { workspace, window, commands } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");
const { pick } = require("./resolve");
const { registerDebugger } = require("./debug");

let client;

function start() {
  const config = workspace.getConfiguration("meadow");
  const configured = config.get("server.path");
  const { command: found, lsp, found: existing } = pick(configured);
  // A bare name is still worth trying when nothing was found on disk: `PATH`
  // may resolve it through a shim we cannot see by joining directory names.
  const command = found || configured || "meadow";

  // An old `meadow` has no `lsp` subcommand, and starting it fails with a
  // message from the argument parser rather than anything about the editor.
  // Say what happened while we still know.
  if (found && !lsp) {
    window
      .showWarningMessage(
        `\`${found}\` does not support \`meadow lsp\` — it is from before the ` +
          "language server existed. Update it (or set `meadow.server.path` to a " +
          "newer build) for diagnostics, hover and go-to-definition; syntax " +
          "highlighting works without it.",
        "Open Settings"
      )
      .then((choice) => {
        if (choice === "Open Settings") {
          commands.executeCommand("workbench.action.openSettings", "meadow.server.path");
        }
      });
  }

  const serverOptions = {
    run: { command, args: ["lsp"], transport: TransportKind.stdio },
    debug: { command, args: ["lsp"], transport: TransportKind.stdio },
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "meadow" }],
    synchronize: { fileEvents: workspace.createFileSystemWatcher("**/*.mw") },
    outputChannelName: "Meadow Language Server",
    middleware: {
      // The "Debug" lens above each definition, unless it has been turned off.
      provideCodeLenses: (document, token, next) =>
        workspace.getConfiguration("meadow").get("debug.codeLens", true) ? next(document, token) : [],
    },
  };

  client = new LanguageClient("meadow", "Meadow Language Server", serverOptions, clientOptions);
  return client.start().catch((err) => {
    const looked = existing.length
      ? `Found, but could not use: ${existing.join(", ")}.`
      : "Found no `meadow` on your PATH, in $MEADOW_HOME/bin, ~/.cargo/bin or ~/.meadow/bin.";
    const hint =
      "Set `meadow.server.path` to the executable — note that a VS Code started " +
      "from the Dock does not inherit your shell's PATH. Syntax highlighting " +
      "still works without the server.";
    window
      .showErrorMessage(
        `Could not start the Meadow language server (\`${command} lsp\`): ` +
          `${err.message}. ${looked} ${hint}`,
        "Open Settings"
      )
      .then((choice) => {
        if (choice === "Open Settings") {
          commands.executeCommand("workbench.action.openSettings", "meadow.server.path");
        }
      });
  });
}

async function stop() {
  if (!client) return;
  // `stop()` throws on a client that never started ("Client is not running and
  // can't be stopped"), which is exactly the state a restart is most useful in.
  try {
    await client.stop();
  } catch {
    // Nothing to stop; dispose of it and start afresh below.
  }
  client = undefined;
}

function activate(context) {
  start();
  registerDebugger(context);
  context.subscriptions.push(
    commands.registerCommand("meadow.restartServer", async () => {
      await stop();
      await start();
      if (client && client.isRunning && client.isRunning()) {
        window.showInformationMessage("Meadow language server restarted.");
      }
    })
  );
}

function deactivate() {
  return stop();
}

module.exports = { activate, deactivate };

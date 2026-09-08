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
const { candidates, resolve } = require("./resolve");

let client;

function start() {
  const config = workspace.getConfiguration("meadow");
  const configured = config.get("server.path");
  // Prefer a path we can see on disk; fall back to the bare name so a `PATH`
  // that does contain it still works.
  const command = resolve(configured) || configured || "meadow";

  const serverOptions = {
    run: { command, args: ["lsp"], transport: TransportKind.stdio },
    debug: { command, args: ["lsp"], transport: TransportKind.stdio },
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "meadow" }],
    synchronize: { fileEvents: workspace.createFileSystemWatcher("**/*.mw") },
    outputChannelName: "Meadow Language Server",
  };

  client = new LanguageClient("meadow", "Meadow Language Server", serverOptions, clientOptions);
  return client.start().catch((err) => {
    const looked = candidates(configured).join(", ");
    window
      .showErrorMessage(
        `Could not start the Meadow language server (\`${command} lsp\`): ${err.message}. ` +
          `Looked for: ${looked}. Set \`meadow.server.path\` to the executable — ` +
          "note that a VS Code started from the Dock does not inherit your shell's PATH. " +
          "Syntax highlighting still works without the server.",
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

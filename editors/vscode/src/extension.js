// The VS Code client: it starts `meadow lsp` and lets the server do the work.
//
// Everything the extension contributes beyond this file is declarative — the
// language id, the TextMate grammar and the indentation rules in
// `package.json` / `language-configuration.json`. Those apply the moment a
// `.mw` file opens, whether or not the server is running, which is why a
// missing `meadow` on PATH degrades to plain syntax highlighting rather than a
// broken editor.

const { workspace, window, commands } = require("vscode");
const { LanguageClient, TransportKind } = require("vscode-languageclient/node");

let client;

function start() {
  const config = workspace.getConfiguration("meadow");
  const command = config.get("server.path") || "meadow";

  const serverOptions = {
    run: { command, args: ["lsp"], transport: TransportKind.stdio },
    debug: { command, args: ["lsp"], transport: TransportKind.stdio },
  };

  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "meadow" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.mw"),
    },
    // The server compiles the standard library once at startup, so the first
    // response can take a moment. Failing to start is worth reporting; a slow
    // first answer is not.
    outputChannelName: "Meadow Language Server",
  };

  client = new LanguageClient("meadow", "Meadow Language Server", serverOptions, clientOptions);
  client.start().catch((err) => {
    window.showErrorMessage(
      `Could not start the Meadow language server (\`${command} lsp\`): ${err.message}. ` +
        "Set `meadow.server.path` if the executable is somewhere else. " +
        "Syntax highlighting still works without it."
    );
  });
}

function activate(context) {
  start();
  context.subscriptions.push(
    commands.registerCommand("meadow.restartServer", async () => {
      if (client) {
        await client.stop();
      }
      start();
      window.showInformationMessage("Meadow language server restarted.");
    })
  );
}

function deactivate() {
  return client ? client.stop() : undefined;
}

module.exports = { activate, deactivate };

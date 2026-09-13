// Debugging: VS Code's side of `meadow dap`.
//
// The adapter does the work -- building the program, running it, answering
// every question about stacks and variables -- and speaks the Debug Adapter
// Protocol over stdio. What is left here is telling VS Code how to start it,
// and making F5 do something sensible before anyone has written a
// `launch.json`.

const vscode = require("vscode");
const fs = require("fs");
const path = require("path");
const { pick, speaksDap } = require("./resolve");

/// The package a file belongs to: the nearest directory above it with a
/// `meadow.toml`. Debugging one module of a package means building all of it,
/// so that is what a launch should name.
function packageRoot(file) {
  let dir = path.dirname(file);
  for (;;) {
    if (fs.existsSync(path.join(dir, "meadow.toml"))) return dir;
    const up = path.dirname(dir);
    if (up === dir) return undefined;
    dir = up;
  }
}

class ConfigurationProvider {
  /// Fill in a launch when there is no `launch.json`, from the open file.
  resolveDebugConfiguration(_folder, config) {
    if (!config.type && !config.request && !config.name) {
      const editor = vscode.window.activeTextEditor;
      if (editor && editor.document.languageId === "meadow") {
        const file = editor.document.fileName;
        config.type = "meadow";
        config.request = "launch";
        config.name = "Debug Meadow";
        config.program = packageRoot(file) || file;
      }
    }
    if (!config.program) {
      return vscode.window
        .showInformationMessage(
          "Open a Meadow file to debug it, or add a `meadow` launch configuration naming a `program`."
        )
        .then(() => undefined);
    }
    return config;
  }
}

class AdapterFactory {
  createDebugAdapterDescriptor() {
    const configured = vscode.workspace.getConfiguration("meadow").get("server.path");
    const { command, lsp: speaks, found } = pick(configured, { probe: speaksDap });
    if (command && !speaks) {
      vscode.window.showErrorMessage(
        `\`${command}\` has no \`meadow dap\` subcommand -- it is from before the debugger ` +
          "existed. Update it, or set `meadow.server.path` to a newer build."
      );
    } else if (!command && found.length === 0) {
      vscode.window.showWarningMessage(
        "Found no `meadow` to debug with; trying `meadow` on the PATH. Set `meadow.server.path` if that fails."
      );
    }
    return new vscode.DebugAdapterExecutable(command || configured || "meadow", ["dap"]);
  }
}

/// The top-level definition the cursor is in, from the language server's
/// "Debug" lenses: the last one that starts at or above the cursor's line.
async function functionAtCursor(editor) {
  const lenses =
    (await vscode.commands.executeCommand(
      "vscode.executeCodeLensProvider",
      editor.document.uri
    )) || [];
  const line = editor.selection.active.line;
  let best;
  for (const lens of lenses) {
    const cmd = lens.command;
    if (!cmd || cmd.command !== "meadow.debugFunction" || !cmd.arguments) continue;
    if (lens.range.start.line <= line && (!best || lens.range.start.line > best.line)) {
      best = cmd.arguments[0];
    }
  }
  return best;
}

/// Ask for arguments, then debug one function: the "Debug" lens above a
/// definition, or "Meadow: Debug Function…" on the one under the cursor.
async function debugFunction(context, target) {
  if (!target) {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.languageId !== "meadow") {
      return vscode.window.showInformationMessage("Put the cursor in a Meadow function to debug it.");
    }
    target = await functionAtCursor(editor);
    if (!target) {
      return vscode.window.showInformationMessage(
        "No top-level definition here to debug. (Is the Meadow language server running?)"
      );
    }
  }

  const uri = vscode.Uri.parse(target.uri);
  const file = uri.fsPath;
  const doc = vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri.toString());
  // The adapter builds from disk, so what is on screen has to be there.
  if (doc && doc.isDirty) await doc.save();

  const key = `meadow.debugArguments:${file}#${target.name}`;
  let expression = target.name;
  if (target.params > 0) {
    const args = await vscode.window.showInputBox({
      title: `Debug ${target.name}`,
      prompt: `${target.name} : ${target.signature}`,
      placeHolder: `${target.params === 1 ? "one argument" : `${target.params} arguments`}, as Meadow expressions, e.g. 42 "text" [1; 2]`,
      value: context.workspaceState.get(key, ""),
      ignoreFocusOut: true,
    });
    if (args === undefined) return;
    await context.workspaceState.update(key, args);
    expression = `${target.name} ${args}`;
  }

  const folder = vscode.workspace.getWorkspaceFolder(uri);
  return vscode.debug.startDebugging(folder, {
    type: "meadow",
    request: "launch",
    name: `Debug ${target.name}`,
    program: packageRoot(file) || file,
    entry: { module: file, expression, function: target.name },
  });
}

function registerDebugger(context) {
  context.subscriptions.push(
    vscode.debug.registerDebugConfigurationProvider("meadow", new ConfigurationProvider()),
    vscode.debug.registerDebugAdapterDescriptorFactory("meadow", new AdapterFactory()),
    vscode.commands.registerCommand("meadow.debugFunction", (target) => debugFunction(context, target))
  );
}

module.exports = { registerDebugger, packageRoot };

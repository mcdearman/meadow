// Running one test: the "Test" lens above a `@test`, and "Meadow: Run Test" on
// the one under the cursor.
//
// It runs `meadow test` itself, as a task, rather than anything of its own. So a
// test run from here is the same build, the same engine and the same output as
// the command line -- and whatever the runner learns to do later, this does too
// without being told.

const vscode = require("vscode");
const { pick, speaksExactTest } = require("./resolve");
const { packageRoot, lensAtCursor } = require("./debug");

async function testFunction(target) {
  if (!target) {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.languageId !== "meadow") {
      return vscode.window.showInformationMessage("Put the cursor in a Meadow `@test` to run it.");
    }
    target = await lensAtCursor(editor, "meadow.testFunction");
    if (!target) {
      return vscode.window.showInformationMessage(
        "No `@test` here to run. (Is the Meadow language server running?)"
      );
    }
  }

  const uri = vscode.Uri.parse(target.uri);
  const file = uri.fsPath;
  const doc = vscode.workspace.textDocuments.find((d) => d.uri.toString() === uri.toString());
  // `meadow test` builds from disk, so what is on screen has to be there.
  if (doc && doc.isDirty) await doc.save();

  const configured = vscode.workspace.getConfiguration("meadow").get("server.path");
  const { command, lsp: speaks } = pick(configured, { probe: speaksExactTest });
  if (command && !speaks) {
    return vscode.window.showErrorMessage(
      `\`${command}\` cannot run a single test -- it has no \`meadow test --exact\`. ` +
        "Update it, or set `meadow.server.path` to a newer build."
    );
  }

  // The whole package, not the file: a test that `use`s a sibling module only
  // builds as part of it. `--exact` on the qualified name, because a bare name
  // is a substring match -- `parse` would run `parseInt` too -- and two modules
  // may each declare a test with the same name.
  const root = packageRoot(file);
  const task = new vscode.Task(
    { type: "meadow", test: target.test },
    vscode.workspace.getWorkspaceFolder(uri) || vscode.TaskScope.Workspace,
    `test ${target.test}`,
    "meadow",
    new vscode.ProcessExecution(command || configured || "meadow", [
      "test",
      root || file,
      "--exact",
      target.test,
    ], { cwd: root || require("path").dirname(file) }),
    []
  );
  task.presentationOptions = {
    reveal: vscode.TaskRevealKind.Always,
    clear: true,
    showReuseMessage: false,
  };
  return vscode.tasks.executeTask(task);
}

function registerTesting(context) {
  context.subscriptions.push(
    vscode.commands.registerCommand("meadow.testFunction", (target) => testFunction(target))
  );
}

module.exports = { registerTesting };

// "Meadow: Show IR": the definition under the cursor in one of the compiler's
// IRs -- core, Cut or AxCut -- in an editor beside the source, with the name
// the cursor is on marked wherever it is in the IR.
//
// The server does the work (`meadow/ir`): it answers the IR's text, what each
// part of it is, and where the name at a position is. This keeps a read-only
// document of that text, asks again as the cursor moves or the source
// changes, and decorates what the answer says to.

const vscode = require("vscode");

const SCHEME = "meadow-ir";
const IRS = [
  { label: "core", description: "the typed IR every pass works on" },
  { label: "cut", description: "the IR a front end hands the back end" },
  { label: "axcut", description: "the IR the runtimes compile" },
];

function registerIr(context, running) {
  // What each IR document shows, by its URI, and which source it is of.
  const shown = new Map();
  const changed = new vscode.EventEmitter();
  const focused = vscode.window.createTextEditorDecorationType({
    backgroundColor: new vscode.ThemeColor("editor.wordHighlightStrongBackground"),
    borderRadius: "2px",
  });

  context.subscriptions.push(
    focused,
    changed,
    vscode.workspace.registerTextDocumentContentProvider(SCHEME, {
      onDidChange: changed.event,
      provideTextDocumentContent: (uri) => (shown.get(uri.toString()) || {}).text || "",
    })
  );

  // One document for each IR of each source file, named so that its tab says
  // which: `Main.mw.cut`.
  const uriFor = (source, ir) =>
    vscode.Uri.from({
      scheme: SCHEME,
      path: `${source.path}.${ir}`,
      query: encodeURIComponent(source.toString()),
    });

  async function refresh(uri, source, ir, position) {
    const client = running();
    if (!client) return false;
    let answer;
    try {
      answer = await client.sendRequest("meadow/ir", {
        textDocument: { uri: source.toString() },
        position: { line: position.line, character: position.character },
        ir,
      });
    } catch (e) {
      return false;
    }
    if (!answer) return false;
    const key = uri.toString();
    const was = shown.get(key);
    shown.set(key, { text: answer.text, focus: answer.focus, source, ir });
    if (!was || was.text !== answer.text) changed.fire(uri);
    mark(key);
    return true;
  }

  function mark(key) {
    const view = shown.get(key);
    if (!view) return;
    for (const editor of vscode.window.visibleTextEditors) {
      if (editor.document.uri.toString() !== key) continue;
      const ranges = view.focus.map(
        (r) => new vscode.Range(r.start.line, r.start.character, r.end.line, r.end.character)
      );
      editor.setDecorations(focused, ranges);
      if (ranges.length > 0) {
        editor.revealRange(ranges[0], vscode.TextEditorRevealType.InCenterIfOutsideViewport);
      }
    }
  }

  // Every IR document open of `source`, asked again at `position`.
  function follow(source, position) {
    for (const [key, view] of shown) {
      if (view.source.toString() === source.toString()) {
        refresh(vscode.Uri.parse(key), view.source, view.ir, position);
      }
    }
  }

  context.subscriptions.push(
    vscode.commands.registerCommand("meadow.showIr", async (which) => {
      const editor = vscode.window.activeTextEditor;
      if (!editor || editor.document.languageId !== "meadow") {
        return vscode.window.showInformationMessage(
          "Put the cursor in a Meadow definition to see it in an IR."
        );
      }
      const ir =
        typeof which === "string"
          ? which
          : ((await vscode.window.showQuickPick(IRS, { placeHolder: "Which IR?" })) || {}).label;
      if (!ir) return;
      const source = editor.document.uri;
      const uri = uriFor(source, ir);
      if (!(await refresh(uri, source, ir, editor.selection.active))) {
        return vscode.window.showInformationMessage(
          "No definition here to show. (Is the Meadow language server running?)"
        );
      }
      const doc = await vscode.workspace.openTextDocument(uri);
      await vscode.window.showTextDocument(doc, {
        viewColumn: vscode.ViewColumn.Beside,
        preserveFocus: true,
        preview: false,
      });
      mark(uri.toString());
    }),
    vscode.window.onDidChangeTextEditorSelection((e) => {
      if (e.textEditor.document.languageId === "meadow") {
        follow(e.textEditor.document.uri, e.selections[0].active);
      }
    }),
    vscode.workspace.onDidChangeTextDocument((e) => {
      const editor = vscode.window.activeTextEditor;
      if (editor && editor.document === e.document && e.document.languageId === "meadow") {
        follow(e.document.uri, editor.selection.active);
      }
    }),
    vscode.window.onDidChangeVisibleTextEditors(() => {
      for (const key of shown.keys()) mark(key);
    }),
    vscode.workspace.onDidCloseTextDocument((doc) => {
      if (doc.uri.scheme === SCHEME) shown.delete(doc.uri.toString());
    })
  );
}

module.exports = { registerIr };

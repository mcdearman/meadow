// Finding a `meadow` that can actually run the language server.
//
// Kept apart from `extension.js` because that file cannot be loaded outside VS
// Code — it requires the `vscode` module — and this is the part worth testing.
//
// There are two ways to get this wrong, and both have happened:
//
//  1. **Not finding it.** A GUI-launched VS Code does not inherit your shell's
//     `PATH`: on macOS it gets the system default unless `launchctl setenv` says
//     otherwise, and neither `cargo install` (`~/.cargo/bin`) nor `install.sh`
//     (`~/.meadow/bin`) writes anywhere on it. So those are searched too, along
//     with `$MEADOW_HOME/bin` if that is set, since both installers honour it.
//
//  2. **Finding the wrong one.** An old `meadow` in an install directory has no
//     `lsp` subcommand, and starting it produces `unrecognized subcommand` —
//     which reaches the user as a language server that silently never works.
//     Two things follow from that: `PATH` is searched *first*, because a
//     `meadow` you put on your `PATH` is the one you meant; and each candidate
//     is asked whether it supports `lsp` before it is chosen.

const cp = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

/// Where `meadow` might be, in the order worth trying.
///
/// An explicit setting is a decision rather than a hint, so it is tried alone.
function candidates(configured, opts = {}) {
  if (configured && configured !== "meadow") return [configured];

  const platform = opts.platform || process.platform;
  const home = opts.home || os.homedir();
  const env = opts.env || process.env;
  const exe = platform === "win32" ? "meadow.exe" : "meadow";
  const sep = platform === "win32" ? ";" : ":";

  const out = [];
  // `PATH` first: what you installed most recently is what you meant.
  const PATH = env.PATH || env.Path || "";
  for (const dir of PATH.split(sep)) {
    if (dir) out.push(path.join(dir, exe));
  }
  // Then the places an installer writes, which a GUI editor's `PATH` may not
  // mention. `MEADOW_HOME` overrides the install directory for both installers.
  if (env.MEADOW_HOME) out.push(path.join(env.MEADOW_HOME, "bin", exe));
  out.push(path.join(home, ".cargo", "bin", exe));
  out.push(path.join(home, ".meadow", "bin", exe));

  return [...new Set(out)];
}

function executable(p) {
  try {
    fs.accessSync(p, fs.constants.X_OK);
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

/// Does this build have the `lsp` subcommand?
///
/// `meadow lsp --help` prints and exits 0 where it exists, and exits non-zero
/// with `unrecognized subcommand` where it does not — so one cheap spawn tells
/// a working install from one that predates the server. Bare `meadow lsp` would
/// not do: it starts the server and waits.
function speaksLsp(exe) {
  try {
    const r = cp.spawnSync(exe, ["lsp", "--help"], {
      timeout: 5000,
      windowsHide: true,
      stdio: "ignore",
    });
    return r.status === 0;
  } catch {
    return false;
  }
}

/// The first candidate that exists on disk, ignoring whether it can serve.
function resolve(configured, opts = {}) {
  const exists = opts.exists || executable;
  for (const c of candidates(configured, opts)) {
    if (path.isAbsolute(c) && exists(c)) return c;
  }
  return undefined;
}

/// Choose what to run.
///
/// Answers `{ command, lsp, found }`:
///
/// * `command` — what to start, or `undefined` if nothing was found on disk (the
///   caller can still try the bare name, in case `PATH` resolves it in a way we
///   cannot see);
/// * `lsp` — whether that binary answered to `meadow lsp`. `false` with a
///   `command` set means an old build was found, which is worth saying out loud
///   rather than letting it fail as a mystery;
/// * `found` — every candidate that exists, for the error message.
function pick(configured, opts = {}) {
  const exists = opts.exists || executable;
  const probe = opts.probe || speaksLsp;

  const found = candidates(configured, opts).filter((c) => path.isAbsolute(c) && exists(c));
  for (const c of found) {
    if (probe(c)) return { command: c, lsp: true, found };
  }
  return { command: found[0], lsp: false, found };
}

module.exports = { candidates, resolve, pick };

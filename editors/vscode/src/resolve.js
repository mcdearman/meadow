// Finding the `meadow` executable.
//
// Kept apart from `extension.js` because that file cannot be loaded outside VS
// Code — it requires the `vscode` module — and this is the part worth testing.

const fs = require("fs");
const os = require("os");
const path = require("path");

/// Where `meadow` ends up, in the order worth trying.
///
/// A GUI-launched VS Code does not inherit your shell's `PATH`: on macOS it gets
/// the system default unless `launchctl setenv` says otherwise, and neither
/// `cargo install` (`~/.cargo/bin`) nor `install.sh` (`~/.meadow/bin`) writes
/// anywhere on it. Searching those two ourselves is the difference between
/// "just works" and an unexplained `startFailed`.
///
/// An explicit setting is a decision rather than a hint, so it is tried alone.
function candidates(configured, home = os.homedir(), platform = process.platform) {
  if (configured && configured !== "meadow") return [configured];
  const exe = platform === "win32" ? "meadow.exe" : "meadow";
  return [
    "meadow",
    path.join(home, ".cargo", "bin", exe),
    path.join(home, ".meadow", "bin", exe),
  ];
}

/// The first candidate that exists and is executable.
///
/// A bare name is skipped rather than rejected — it may resolve on a `PATH` we
/// cannot see, so it stays the fallback if nothing on disk matches.
function resolve(configured, opts = {}) {
  const exists = opts.exists || ((p) => {
    try {
      fs.accessSync(p, fs.constants.X_OK);
      return true;
    } catch {
      return false;
    }
  });
  for (const c of candidates(configured, opts.home, opts.platform)) {
    if (path.isAbsolute(c) && exists(c)) return c;
  }
  return undefined;
}

module.exports = { candidates, resolve };

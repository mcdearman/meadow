// Run with `node --test` (no dependencies, no VS Code).
const { test } = require("node:test");
const assert = require("node:assert");
const path = require("path");
const { candidates, resolve, pick } = require("../src/resolve");

// Built with `path.join`, like the code under test: these tests simulate other
// platforms but run on this one, so the separator is whatever the host uses.
const HOME = "/home/u";
const cargo = path.join(HOME, ".cargo", "bin", "meadow");
const meadowHome = path.join(HOME, ".meadow", "bin", "meadow");
const onPath = path.join("/usr/local/bin", "meadow");
const alsoOnPath = path.join("/usr/bin", "meadow");

/// A fixed environment, so the tests do not depend on the machine running them.
const env = (extra = {}) => ({ PATH: "/usr/local/bin:/usr/bin", ...extra });
const opts = (extra = {}) => ({ home: HOME, platform: "darwin", env: env(), ...extra });

test("an explicit setting is tried alone", () => {
  assert.deepEqual(candidates("/opt/meadow", opts()), ["/opt/meadow"]);
});

test("PATH comes before the install directories", () => {
  // The ordering that matters: a `meadow` you put on your PATH is the one you
  // meant, and an install directory may hold something much older.
  const c = candidates("meadow", opts());
  assert.deepEqual(c, [onPath, alsoOnPath, cargo, meadowHome]);
  assert.deepEqual(candidates(undefined, opts()), c);
});

test("MEADOW_HOME is searched, since both installers honour it", () => {
  const c = candidates("meadow", opts({ env: env({ MEADOW_HOME: "/opt/mw" }) }));
  assert.ok(c.includes(path.join("/opt/mw", "bin", "meadow")), c.join(","));
  // ...and before the defaults, because setting it is a decision.
  assert.ok(c.indexOf(path.join("/opt/mw", "bin", "meadow")) < c.indexOf(cargo));
});

test("windows looks for the .exe and splits PATH on ;", () => {
  const c = candidates("meadow", {
    home: "C:\\Users\\u",
    platform: "win32",
    env: { PATH: "C:\\bin;C:\\other" },
  });
  assert.ok(
    c.every((p) => p.endsWith("meadow.exe")),
    c.join(",")
  );
  assert.ok(c.includes(path.join("C:\\bin", "meadow.exe")), c.join(","));
});

test("a bare name is never returned as a path", () => {
  // Nothing on disk: the caller falls back to letting the system resolve it.
  assert.equal(resolve("meadow", opts({ exists: () => false })), undefined);
});

test("an explicit setting that does not exist is not silently replaced", () => {
  // Better to fail naming what was asked for than to start a different binary.
  const found = resolve("/opt/meadow", opts({ exists: (p) => p === cargo }));
  assert.equal(found, undefined);
});

// --- choosing one that can actually serve --------------------------------

test("a build without `lsp` is skipped in favour of one with it", () => {
  // The failure this exists for: an old `meadow` sitting in ~/.meadow/bin,
  // found first, started, and failing with `unrecognized subcommand`.
  const chosen = pick("meadow", {
    ...opts(),
    exists: (p) => p === meadowHome || p === onPath,
    probe: (p) => p === onPath,
  });
  assert.equal(chosen.command, onPath);
  assert.equal(chosen.lsp, true);
});

test("an old build is still reported, so the message can say why", () => {
  const chosen = pick("meadow", {
    ...opts(),
    exists: (p) => p === meadowHome,
    probe: () => false,
  });
  assert.equal(chosen.command, meadowHome);
  assert.equal(chosen.lsp, false, "the caller has to be able to tell");
  assert.deepEqual(chosen.found, [meadowHome]);
});

test("nothing on disk leaves the command unset", () => {
  const chosen = pick("meadow", { ...opts(), exists: () => false, probe: () => true });
  assert.equal(chosen.command, undefined);
  assert.deepEqual(chosen.found, []);
});

test("a candidate is only probed once it is known to exist", () => {
  const probed = [];
  pick("meadow", {
    ...opts(),
    exists: (p) => p === cargo,
    probe: (p) => {
      probed.push(p);
      return true;
    },
  });
  assert.deepEqual(probed, [cargo]);
});

test("it agrees with itself on this machine", () => {
  // No assertion about *what* is installed — only that the two entry points do
  // not contradict each other, whatever this machine has.
  const first = resolve("meadow");
  const chosen = pick("meadow");
  if (chosen.command) {
    assert.ok(path.isAbsolute(chosen.command), chosen.command);
    assert.ok(chosen.found.includes(first), `${first} vs ${chosen.found.join(",")}`);
  } else {
    assert.equal(first, undefined);
  }
});

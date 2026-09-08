// Run with `node --test` (no dependencies, no VS Code).
const { test } = require("node:test");
const assert = require("node:assert");
const path = require("path");
const { candidates, resolve } = require("../src/resolve");

const HOME = "/home/u";
const cargo = path.join(HOME, ".cargo", "bin", "meadow");
const meadowHome = path.join(HOME, ".meadow", "bin", "meadow");

test("an explicit setting is tried alone", () => {
  assert.deepEqual(candidates("/opt/meadow", HOME, "darwin"), ["/opt/meadow"]);
});

test("the default searches PATH then the two install directories", () => {
  assert.deepEqual(candidates("meadow", HOME, "darwin"), ["meadow", cargo, meadowHome]);
  assert.deepEqual(candidates(undefined, HOME, "darwin"), ["meadow", cargo, meadowHome]);
});

test("windows looks for the .exe", () => {
  const c = candidates("meadow", "C:\\Users\\u", "win32");
  assert.ok(c.every((p) => p === "meadow" || p.endsWith("meadow.exe")), c.join(","));
});

test("cargo's copy wins when both are installed", () => {
  const found = resolve("meadow", {
    home: HOME,
    platform: "darwin",
    exists: (p) => p === cargo || p === meadowHome,
  });
  assert.equal(found, cargo);
});

test("install.sh's copy is found when cargo's is not there", () => {
  const found = resolve("meadow", {
    home: HOME,
    platform: "darwin",
    exists: (p) => p === meadowHome,
  });
  assert.equal(found, meadowHome);
});

test("a bare name is never returned as a path", () => {
  // Nothing on disk: the caller falls back to letting the system resolve it.
  const found = resolve("meadow", { home: HOME, platform: "darwin", exists: () => false });
  assert.equal(found, undefined);
});

test("an explicit setting that does not exist is not silently replaced", () => {
  // Better to fail naming what was asked for than to start a different binary.
  const found = resolve("/opt/meadow", {
    home: HOME,
    platform: "darwin",
    exists: (p) => p === cargo,
  });
  assert.equal(found, undefined);
});

test("it finds the real executable on this machine", () => {
  const found = resolve("meadow");
  assert.ok(found === undefined || path.isAbsolute(found), `got ${found}`);
});

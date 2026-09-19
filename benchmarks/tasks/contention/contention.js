// Threads contending for shared mutable state: eight workers moving money
// between sixteen accounts, every transfer reading two accounts and writing
// two as one indivisible step.
//
// JavaScript has no shared mutable object across workers, so the accounts are a
// SharedArrayBuffer and the indivisible step is held by a lock built out of
// `Atomics.wait`. This is the least idiomatic program in the suite, and that is
// the finding: the language has no comfortable answer here.

const { Worker, isMainThread, parentPort, workerData } = require("worker_threads");

const ACCOUNTS = 16;
const WORKERS = 8;
const MOVES = 20000;

function acquire(gate) {
  while (Atomics.compareExchange(gate, 0, 0, 1) !== 0) Atomics.wait(gate, 0, 1);
}

function release(gate) {
  Atomics.store(gate, 0, 0);
  Atomics.notify(gate, 0, 1);
}

if (isMainThread) {
  const shared = new SharedArrayBuffer(8 * ACCOUNTS);
  const gateBuf = new SharedArrayBuffer(4);
  const bank = new BigInt64Array(shared);
  for (let i = 0; i < ACCOUNTS; i++) bank[i] = 1000n;
  let left = WORKERS;
  for (let w = 0; w < WORKERS; w++) {
    const worker = new Worker(__filename, { workerData: { shared, gateBuf, seed: w + 1 } });
    worker.on("exit", () => {
      if (--left === 0) console.log(Array.from(bank, (b) => b.toString()).join(","));
    });
  }
} else {
  const bank = new BigInt64Array(workerData.shared);
  const gate = new Int32Array(workerData.gateBuf);
  let s = workerData.seed;
  for (let i = 0; i < MOVES; i++) {
    s = (s * 48271) % 2147483647;
    const a = s % ACCOUNTS;
    const b = Math.floor(s / ACCOUNTS) % ACCOUNTS;
    const amount = BigInt(1 + (s % 10));
    if (a === b) continue;
    acquire(gate);
    bank[a] -= amount;
    bank[b] += amount;
    release(gate);
  }
  parentPort.postMessage("done");
}

// Message passing: four producer workers each send fifty thousand numbers to
// the main thread, which adds them up.
//
// `postMessage` is the only channel between JavaScript workers, and it copies
// rather than hands over. Sending two hundred thousand of anything through it
// is not what the design is for, and the timing says so.

const { Worker, isMainThread, parentPort, workerData } = require("worker_threads");

const PRODUCERS = 4;
const EACH = 50000;

if (isMainThread) {
  let total = 0;
  let left = PRODUCERS;
  for (let p = 0; p < PRODUCERS; p++) {
    const worker = new Worker(__filename, { workerData: { base: p * EACH } });
    worker.on("message", (v) => {
      total += v;
    });
    worker.on("exit", () => {
      if (--left === 0) console.log(total);
    });
  }
} else {
  for (let i = 0; i < EACH; i++) parentPort.postMessage(workerData.base + i);
}

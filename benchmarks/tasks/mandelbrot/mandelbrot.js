// Data parallelism: a 2000x2000 grid of independent float work, split between
// workers by taking every Nth row.
//
// `worker_threads`: JavaScript has one thread per isolate, so parallelism means
// more isolates, and the file is both the main program and the worker.

const os = require("os");
const { Worker, isMainThread, parentPort, workerData } = require("worker_threads");

const SIDE = 2000;
const LIMIT = 100;

function band(start, step) {
  let total = 0;
  for (let j = start; j < SIDE; j += step) {
    const cy = (j / SIDE) * 3.0 - 1.5;
    for (let i = 0; i < SIDE; i++) {
      const cx = (i / SIDE) * 3.0 - 2.0;
      let x = 0.0, y = 0.0, n = 0;
      while (n < LIMIT && x * x + y * y <= 4.0) {
        const nx = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = nx;
        n++;
      }
      total += n;
    }
  }
  return total;
}

if (isMainThread) {
  const workers = os.availableParallelism ? os.availableParallelism() : os.cpus().length;
  let left = workers;
  let total = 0;
  for (let w = 0; w < workers; w++) {
    const worker = new Worker(__filename, { workerData: { start: w, step: workers } });
    worker.on("message", (sum) => {
      total += sum;
      if (--left === 0) console.log(total);
    });
  }
} else {
  parentPort.postMessage(band(workerData.start, workerData.step));
}

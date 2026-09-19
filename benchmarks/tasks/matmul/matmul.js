// Three nested loops over flat typed arrays of doubles: the shape of numerical
// code, and of nothing else. `i k j` order, so the innermost loop walks in order.

const N = 256;
const a = new Float64Array(N * N);
const b = new Float64Array(N * N);
const c = new Float64Array(N * N);

for (let i = 0; i < N; i++) {
  for (let j = 0; j < N; j++) {
    a[i * N + j] = (i + j) % 10;
    b[i * N + j] = (i * j) % 10;
  }
}
for (let i = 0; i < N; i++) {
  for (let k = 0; k < N; k++) {
    const aik = a[i * N + k];
    for (let j = 0; j < N; j++) {
      c[i * N + j] += aik * b[k * N + j];
    }
  }
}
let total = 0;
for (let i = 0; i < N; i++) total += c[i * N + i];
console.log(total);

// `wordfreq`, with an arena: the same counts, with nothing allocated per word.
//
// The corpus is read once into one Buffer, and a word is a pair of numbers into
// it -- where it starts and how long it is -- so no word is ever copied. The
// table is three typed arrays of a fixed size rather than a Map, and probing
// writes numbers into them. What is left in the number is the hashing, the
// probing and the byte comparisons, with the allocator and the string type
// taken out.
//
// Only the ten reported at the end become strings.

const fs = require("fs");

const CAP = 1 << 14; // 16384 slots for a vocabulary of 5000
const starts = new Int32Array(CAP);
const lens = new Int32Array(CAP);
const counts = new Float64Array(CAP);
const text = fs.readFileSync("work/corpus.txt");

// FNV-1a, in 32 bits: JavaScript has no 64-bit integer arithmetic that is
// quick, and a 32-bit hash probes just as well over 16384 slots.
function hashOf(at, n) {
  let h = 2166136261;
  for (let i = 0; i < n; i++) {
    h ^= text[at + i];
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function same(a, b, n) {
  for (let i = 0; i < n; i++) if (text[a + i] !== text[b + i]) return false;
  return true;
}

function bump(at, n) {
  let i = hashOf(at, n) & (CAP - 1);
  for (;;) {
    if (counts[i] === 0) {
      starts[i] = at;
      lens[i] = n;
      counts[i] = 1;
      return;
    }
    if (lens[i] === n && same(starts[i], at, n)) {
      counts[i]++;
      return;
    }
    i = (i + 1) & (CAP - 1);
  }
}

// Is the word in slot a before the one in slot b, count first then bytes?
function before(a, b) {
  if (counts[a] !== counts[b]) return counts[a] > counts[b];
  const n = Math.min(lens[a], lens[b]);
  for (let i = 0; i < n; i++) {
    const x = text[starts[a] + i], y = text[starts[b] + i];
    if (x !== y) return x < y;
  }
  return lens[a] < lens[b];
}

for (let i = 0; i < text.length; ) {
  while (i < text.length && (text[i] === 32 || text[i] === 10)) i++;
  const start = i;
  while (i < text.length && text[i] !== 32 && text[i] !== 10) i++;
  if (i > start) bump(start, i - start);
}

// The ten commonest, kept in order as the table is walked.
const top = [];
for (let i = 0; i < CAP; i++) {
  if (counts[i] === 0) continue;
  let at = top.length;
  while (at > 0 && before(i, top[at - 1])) at--;
  if (at >= 10) continue;
  top.splice(at, 0, i);
  if (top.length > 10) top.length = 10;
}
console.log(
  top
    .map((i) => `${text.toString("latin1", starts[i], starts[i] + lens[i])}:${counts[i]}`)
    .join(" "),
);

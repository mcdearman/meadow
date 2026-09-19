// Strings, a hash map and a sort: count the words of a 3MB file and report the
// ten commonest, count first and then the word.

const fs = require("fs");

const text = fs.readFileSync("work/corpus.txt", "utf8");
const counts = new Map();
for (const word of text.split(/\s+/)) {
  if (word.length === 0) continue;
  counts.set(word, (counts.get(word) ?? 0) + 1);
}
const ranked = [...counts].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
console.log(ranked.slice(0, 10).map(([w, c]) => `${w}:${c}`).join(" "));

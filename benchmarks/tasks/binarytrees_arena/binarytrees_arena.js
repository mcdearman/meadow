// `binarytrees`, with an arena: the same trees, built into one flat block of
// nodes and thrown away whole.
//
// A node is three slots -- value, left, right -- and a child is the index of
// one, not an object. Building a tree is a bump of a counter per node;
// discarding one is setting the counter back to nought. What is left in the
// number is writing the nodes and walking them, with the allocator and the
// collector taken out.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

const TIP = -1;
const top = 18;
// Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots each.
const room = 3 * (1 << (top + 1));

let bump = 0;

function build(a, v, d) {
  const i = bump;
  bump += 3;
  if (d <= 0) {
    a[i] = v;
    a[i + 1] = TIP;
    return i;
  }
  const l = build(a, v, d - 1);
  const r = build(a, v + 1, d - 1);
  a[i] = 0;
  a[i + 1] = l;
  a[i + 2] = r;
  return i;
}

function check(a, i) {
  return a[i + 1] === TIP ? a[i] : check(a, a[i + 1]) + check(a, a[i + 2]);
}

const lasting = new Float64Array(room);
const arena = new Float64Array(room);
const root = build(lasting, 1, top);

let total = 0;
for (let d = 4; d <= top; d += 2) {
  for (let i = 1 << (top - d + 4); i > 0; i--) {
    bump = 0;
    total += check(arena, build(arena, i, d));
  }
}
console.log(total + check(lasting, root));

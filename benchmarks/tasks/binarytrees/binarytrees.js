// Allocation and collection: build a small tree, walk it, discard it, tens of
// millions of nodes over, with one long-lived tree kept alive throughout.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

function build(v, d) {
  return d <= 0 ? { left: null, right: null, value: v }
                : { left: build(v, d - 1), right: build(v + 1, d - 1), value: 0 };
}

function check(t) {
  return t.left === null ? t.value : check(t.left) + check(t.right);
}

const top = 18;
const lasting = build(1, top);
let total = 0;
for (let d = 4; d <= top; d += 2) {
  for (let i = 1 << (top - d + 4); i > 0; i--) {
    total += check(build(i, d));
  }
}
console.log(total + check(lasting));

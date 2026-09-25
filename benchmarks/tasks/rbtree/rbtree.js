// Koka's rbtree: 4.2 million keys into a functional red-black tree. Every
// step down the tree builds a new node; null is a leaf.

const RED = 0;
const BLACK = 1;

function node(color, left, key, val, right) {
  return { color, left, key, val, right };
}

function isRed(t) {
  return t !== null && t.color === RED;
}

function balanceLeft(l, k, v, r) {
  if (l === null) return null;
  if (isRed(l.left)) {
    const x = l.left;
    return node(RED, node(BLACK, x.left, x.key, x.val, x.right), l.key, l.val, node(BLACK, l.right, k, v, r));
  }
  if (isRed(l.right)) {
    const x = l.right;
    return node(RED, node(BLACK, l.left, l.key, l.val, x.left), x.key, x.val, node(BLACK, x.right, k, v, r));
  }
  return node(BLACK, node(RED, l.left, l.key, l.val, l.right), k, v, r);
}

function balanceRight(l, k, v, r) {
  if (r === null) return null;
  if (isRed(r.left)) {
    const x = r.left;
    return node(RED, node(BLACK, l, k, v, x.left), x.key, x.val, node(BLACK, x.right, r.key, r.val, r.right));
  }
  if (isRed(r.right)) {
    const x = r.right;
    return node(RED, node(BLACK, l, k, v, r.left), r.key, r.val, node(BLACK, x.left, x.key, x.val, x.right));
  }
  return node(BLACK, l, k, v, node(RED, r.left, r.key, r.val, r.right));
}

function ins(t, k, v) {
  if (t === null) return node(RED, null, k, v, null);
  if (t.color === RED) {
    if (k < t.key) return node(RED, ins(t.left, k, v), t.key, t.val, t.right);
    if (k > t.key) return node(RED, t.left, t.key, t.val, ins(t.right, k, v));
    return node(RED, t.left, k, v, t.right);
  }
  if (k < t.key) {
    if (isRed(t.left)) return balanceLeft(ins(t.left, k, v), t.key, t.val, t.right);
    return node(BLACK, ins(t.left, k, v), t.key, t.val, t.right);
  }
  if (k > t.key) {
    if (isRed(t.right)) return balanceRight(t.left, t.key, t.val, ins(t.right, k, v));
    return node(BLACK, t.left, t.key, t.val, ins(t.right, k, v));
  }
  return node(BLACK, t.left, k, v, t.right);
}

function insert(t, k, v) {
  const u = ins(t, k, v);
  return node(BLACK, u.left, u.key, u.val, u.right);
}

function fold(t, b, f) {
  while (t !== null) {
    b = f(t.key, t.val, fold(t.left, b, f));
    t = t.right;
  }
  return b;
}

let t = null;
for (let n = 4200000; n > 0; ) {
  n--;
  t = insert(t, n, n % 10 === 0);
}
console.log(fold(t, 0, (k, v, r) => (v ? r + 1 : r)));

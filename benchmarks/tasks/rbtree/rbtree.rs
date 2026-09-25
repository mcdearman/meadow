// Koka's rbtree: 4.2 million keys into a functional red-black tree. The tree
// is owned, so each node is taken apart by value and its box used again for
// the new one -- what Perceus does by counting, Rust does by moving.

#[derive(Clone, Copy, PartialEq)]
enum Color {
    Red,
    Black,
}

use Color::*;

type Tree = Option<Box<Node>>;

struct Node {
    color: Color,
    left: Tree,
    key: i32,
    val: bool,
    right: Tree,
}

fn node(color: Color, left: Tree, key: i32, val: bool, right: Tree) -> Tree {
    Some(Box::new(Node {
        color,
        left,
        key,
        val,
        right,
    }))
}

fn is_red(t: &Tree) -> bool {
    matches!(t, Some(n) if n.color == Red)
}

fn balance_left(l: Tree, k: i32, v: bool, r: Tree) -> Tree {
    let Some(l) = l else { return None };
    let Node {
        left: ll,
        key: lk,
        val: lv,
        right: lr,
        ..
    } = *l;
    if is_red(&ll) {
        let x = ll.unwrap();
        node(
            Red,
            node(Black, x.left, x.key, x.val, x.right),
            lk,
            lv,
            node(Black, lr, k, v, r),
        )
    } else if is_red(&lr) {
        let x = lr.unwrap();
        node(
            Red,
            node(Black, ll, lk, lv, x.left),
            x.key,
            x.val,
            node(Black, x.right, k, v, r),
        )
    } else {
        node(Black, node(Red, ll, lk, lv, lr), k, v, r)
    }
}

fn balance_right(l: Tree, k: i32, v: bool, r: Tree) -> Tree {
    let Some(r) = r else { return None };
    let Node {
        left: rl,
        key: rk,
        val: rv,
        right: rr,
        ..
    } = *r;
    if is_red(&rl) {
        let x = rl.unwrap();
        node(
            Red,
            node(Black, l, k, v, x.left),
            x.key,
            x.val,
            node(Black, x.right, rk, rv, rr),
        )
    } else if is_red(&rr) {
        let x = rr.unwrap();
        node(
            Red,
            node(Black, l, k, v, rl),
            rk,
            rv,
            node(Black, x.left, x.key, x.val, x.right),
        )
    } else {
        node(Black, l, k, v, node(Red, rl, rk, rv, rr))
    }
}

fn ins(t: Tree, k: i32, v: bool) -> Tree {
    let Some(mut n) = t else {
        return node(Red, None, k, v, None);
    };
    if n.color == Red {
        if k < n.key {
            n.left = ins(n.left.take(), k, v);
        } else if k > n.key {
            n.right = ins(n.right.take(), k, v);
        } else {
            n.val = v;
        }
        Some(n)
    } else if k < n.key {
        if is_red(&n.left) {
            let Node {
                left,
                key,
                val,
                right,
                ..
            } = *n;
            balance_left(ins(left, k, v), key, val, right)
        } else {
            n.left = ins(n.left.take(), k, v);
            Some(n)
        }
    } else if k > n.key {
        if is_red(&n.right) {
            let Node {
                left,
                key,
                val,
                right,
                ..
            } = *n;
            balance_right(left, key, val, ins(right, k, v))
        } else {
            n.right = ins(n.right.take(), k, v);
            Some(n)
        }
    } else {
        n.val = v;
        Some(n)
    }
}

fn insert(t: Tree, k: i32, v: bool) -> Tree {
    let mut t = ins(t, k, v);
    if let Some(n) = &mut t {
        n.color = Black;
    }
    t
}

fn fold<A>(t: &Tree, b: A, f: &impl Fn(i32, bool, A) -> A) -> A {
    match t {
        Some(n) => {
            let b = fold(&n.left, b, f);
            fold(&n.right, f(n.key, n.val, b), f)
        }
        None => b,
    }
}

fn main() {
    let mut t: Tree = None;
    let mut n = 4_200_000;
    while n > 0 {
        n -= 1;
        t = insert(t, n, n % 10 == 0);
    }
    println!("{}", fold(&t, 0, &|_, v, r| if v { r + 1 } else { r }));
}

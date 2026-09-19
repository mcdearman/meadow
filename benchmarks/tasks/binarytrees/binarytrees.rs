// Allocation and collection: build a small tree, walk it, discard it, tens of
// millions of nodes over, with one long-lived tree kept alive throughout.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

enum Tree {
    Tip(i64),
    Fork(Box<Tree>, Box<Tree>),
}

fn build(v: i64, d: i32) -> Tree {
    if d <= 0 {
        Tree::Tip(v)
    } else {
        Tree::Fork(Box::new(build(v, d - 1)), Box::new(build(v + 1, d - 1)))
    }
}

fn check(t: &Tree) -> i64 {
    match t {
        Tree::Tip(v) => *v,
        Tree::Fork(l, r) => check(l) + check(r),
    }
}

fn main() {
    let top = 18;
    let lasting = build(1, top);
    let mut total = 0i64;
    let mut d = 4;
    while d <= top {
        for i in (1..=1i64 << (top - d + 4)).rev() {
            total += check(&build(i, d));
        }
        d += 2;
    }
    println!("{}", total + check(&lasting));
}

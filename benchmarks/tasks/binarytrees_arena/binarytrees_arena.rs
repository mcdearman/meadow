// `binarytrees`, with an arena: the same trees, built into one flat block of
// nodes and thrown away whole.
//
// A node is three slots -- value, left, right -- and a child is the index of
// one, not a `Box`. Building a tree is a bump of a counter per node;
// discarding one is setting the counter back to nought. What is left in the
// number is writing the nodes and walking them, with the allocator taken out.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

const TIP: i64 = -1;

fn build(a: &mut [i64], n: &mut usize, v: i64, d: i32) -> i64 {
    let i = *n;
    *n += 3;
    if d <= 0 {
        a[i] = v;
        a[i + 1] = TIP;
        return i as i64;
    }
    let l = build(a, n, v, d - 1);
    let r = build(a, n, v + 1, d - 1);
    a[i] = 0;
    a[i + 1] = l;
    a[i + 2] = r;
    i as i64
}

fn check(a: &[i64], i: i64) -> i64 {
    let i = i as usize;
    if a[i + 1] == TIP {
        return a[i];
    }
    check(a, a[i + 1]) + check(a, a[i + 2])
}

fn main() {
    let top = 18;
    // Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots each.
    let room = 3 * (1usize << (top + 1));
    let mut lasting = vec![0i64; room];
    let mut lasting_n = 0usize;
    let root = build(&mut lasting, &mut lasting_n, 1, top);

    let mut arena = vec![0i64; room];
    let mut total = 0i64;
    let mut d = 4;
    while d <= top {
        for i in (1..=1i64 << (top - d + 4)).rev() {
            let mut bump = 0usize;
            let t = build(&mut arena, &mut bump, i, d);
            total += check(&arena, t);
        }
        d += 2;
    }
    println!("{}", total + check(&lasting, root));
}

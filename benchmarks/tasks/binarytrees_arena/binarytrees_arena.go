// `binarytrees`, with an arena: the same trees, built into one flat block of
// nodes and thrown away whole.
//
// A node is three slots -- value, left, right -- and a child is the index of
// one, not a pointer. Building a tree is a bump of a counter per node;
// discarding one is setting the counter back to nought. What is left in the
// number is writing the nodes and walking them, with the allocator and the
// collector taken out.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

package main

import "fmt"

const tip = -1

func build(a []int64, n *int64, v int64, d int) int64 {
	i := *n
	*n += 3
	if d <= 0 {
		a[i] = v
		a[i+1] = tip
		return i
	}
	l := build(a, n, v, d-1)
	r := build(a, n, v+1, d-1)
	a[i] = 0
	a[i+1] = l
	a[i+2] = r
	return i
}

func check(a []int64, i int64) int64 {
	if a[i+1] == tip {
		return a[i]
	}
	return check(a, a[i+1]) + check(a, a[i+2])
}

func main() {
	const top = 18
	// Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots each.
	room := 3 * (int64(1) << (top + 1))
	lasting := make([]int64, room)
	var lastingN int64
	root := build(lasting, &lastingN, 1, top)

	arena := make([]int64, room)
	var total int64
	for d := 4; d <= top; d += 2 {
		for i := int64(1) << (top - d + 4); i > 0; i-- {
			var bump int64
			t := build(arena, &bump, i, d)
			total += check(arena, t)
		}
	}
	fmt.Println(total + check(lasting, root))
}

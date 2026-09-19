// Allocation and collection: build a small tree, walk it, discard it, tens of
// millions of nodes over, with one long-lived tree kept alive throughout.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

package main

import "fmt"

type Tree struct {
	left, right *Tree
	value       int64
}

func build(v int64, d int) *Tree {
	if d <= 0 {
		return &Tree{value: v}
	}
	return &Tree{left: build(v, d-1), right: build(v+1, d-1)}
}

func check(t *Tree) int64 {
	if t.left == nil {
		return t.value
	}
	return check(t.left) + check(t.right)
}

func main() {
	const top = 18
	lasting := build(1, top)
	var total int64
	for d := 4; d <= top; d += 2 {
		for i := int64(1) << (top - d + 4); i > 0; i-- {
			total += check(build(i, d))
		}
	}
	fmt.Println(total + check(lasting))
}

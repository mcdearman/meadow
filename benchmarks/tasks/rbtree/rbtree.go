// Koka's rbtree: 4.2 million keys into a functional red-black tree. Every
// step down the tree builds a new node, as the Java and Haskell ports do; the
// collector takes back the old ones.
package main

import "fmt"

type Color int

const (
	Red Color = iota
	Black
)

type Tree struct {
	color Color
	left  *Tree
	key   int32
	val   bool
	right *Tree
}

func node(c Color, l *Tree, k int32, v bool, r *Tree) *Tree {
	return &Tree{c, l, k, v, r}
}

func isRed(t *Tree) bool { return t != nil && t.color == Red }

func balanceLeft(l *Tree, k int32, v bool, r *Tree) *Tree {
	if l == nil {
		return nil
	}
	if isRed(l.left) {
		x := l.left
		return node(Red, node(Black, x.left, x.key, x.val, x.right), l.key, l.val, node(Black, l.right, k, v, r))
	}
	if isRed(l.right) {
		x := l.right
		return node(Red, node(Black, l.left, l.key, l.val, x.left), x.key, x.val, node(Black, x.right, k, v, r))
	}
	return node(Black, node(Red, l.left, l.key, l.val, l.right), k, v, r)
}

func balanceRight(l *Tree, k int32, v bool, r *Tree) *Tree {
	if r == nil {
		return nil
	}
	if isRed(r.left) {
		x := r.left
		return node(Red, node(Black, l, k, v, x.left), x.key, x.val, node(Black, x.right, r.key, r.val, r.right))
	}
	if isRed(r.right) {
		x := r.right
		return node(Red, node(Black, l, k, v, r.left), r.key, r.val, node(Black, x.left, x.key, x.val, x.right))
	}
	return node(Black, l, k, v, node(Red, r.left, r.key, r.val, r.right))
}

func ins(t *Tree, k int32, v bool) *Tree {
	if t == nil {
		return node(Red, nil, k, v, nil)
	}
	if t.color == Red {
		if k < t.key {
			return node(Red, ins(t.left, k, v), t.key, t.val, t.right)
		} else if k > t.key {
			return node(Red, t.left, t.key, t.val, ins(t.right, k, v))
		}
		return node(Red, t.left, k, v, t.right)
	}
	if k < t.key {
		if isRed(t.left) {
			return balanceLeft(ins(t.left, k, v), t.key, t.val, t.right)
		}
		return node(Black, ins(t.left, k, v), t.key, t.val, t.right)
	} else if k > t.key {
		if isRed(t.right) {
			return balanceRight(t.left, t.key, t.val, ins(t.right, k, v))
		}
		return node(Black, t.left, t.key, t.val, ins(t.right, k, v))
	}
	return node(Black, t.left, k, v, t.right)
}

func insert(t *Tree, k int32, v bool) *Tree {
	t = ins(t, k, v)
	return node(Black, t.left, t.key, t.val, t.right)
}

func fold(t *Tree, b int, f func(int32, bool, int) int) int {
	for t != nil {
		b = f(t.key, t.val, fold(t.left, b, f))
		t = t.right
	}
	return b
}

func main() {
	var t *Tree
	for n := int32(4200000); n > 0; {
		n--
		t = insert(t, n, n%10 == 0)
	}
	fmt.Println(fold(t, 0, func(_ int32, v bool, r int) int {
		if v {
			return r + 1
		}
		return r
	}))
}

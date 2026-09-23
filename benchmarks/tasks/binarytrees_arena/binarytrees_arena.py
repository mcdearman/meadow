"""`binarytrees`, with an arena: the same trees, built into one flat block of
nodes and thrown away whole.

A node is three slots -- value, left, right -- and a child is the index of
one, not a tuple. Building a tree is a bump of a counter per node; discarding
one is setting the counter back to nought. What is left in the number is
writing the nodes and walking them, with the allocator and the collector taken
out.

Each tip carries the iteration it was built in, so no two trees are alike."""

import sys
from array import array

TIP = -1


def build(a, n, v, d):
    i = n[0]
    n[0] = i + 3
    if d <= 0:
        a[i] = v
        a[i + 1] = TIP
        return i
    l = build(a, n, v, d - 1)
    r = build(a, n, v + 1, d - 1)
    a[i] = 0
    a[i + 1] = l
    a[i + 2] = r
    return i


def check(a, i):
    if a[i + 1] == TIP:
        return a[i]
    return check(a, a[i + 1]) + check(a, a[i + 2])


def main():
    top = 18
    # Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots each.
    room = 3 * (1 << (top + 1))
    lasting = array("q", bytes(8 * room))
    root = build(lasting, [0], 1, top)

    arena = array("q", bytes(8 * room))
    bump = [0]
    total = 0
    for d in range(4, top + 1, 2):
        for i in range(1 << (top - d + 4), 0, -1):
            bump[0] = 0
            total += check(arena, build(arena, bump, i, d))
    print(total + check(lasting, root))


if __name__ == "__main__":
    sys.setrecursionlimit(10000)
    main()

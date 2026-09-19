"""Allocation and collection: build a small tree, walk it, discard it, tens of
millions of nodes over, with one long-lived tree kept alive throughout.

A tip is a one-tuple and a fork a two-tuple -- what Python reaches for before
it reaches for a class.

Each tip carries the iteration it was built in, so no two trees are alike and
no compiler can build one and reuse it."""

import sys


def build(v, d):
    return (v,) if d <= 0 else (build(v, d - 1), build(v + 1, d - 1))


def check(t):
    return t[0] if len(t) == 1 else check(t[0]) + check(t[1])


def main():
    top = 18
    lasting = build(1, top)
    total = 0
    for d in range(4, top + 1, 2):
        for i in range(1 << (top - d + 4), 0, -1):
            total += check(build(i, d))
    print(total + check(lasting))


if __name__ == "__main__":
    sys.setrecursionlimit(10000)
    main()

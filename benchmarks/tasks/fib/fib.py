"""fib n, the naive way: two calls and an addition per node, nothing else."""

import sys


def fib(n):
    return n if n < 2 else fib(n - 1) + fib(n - 2)


if __name__ == "__main__":
    sys.setrecursionlimit(10000)
    print(fib(32))

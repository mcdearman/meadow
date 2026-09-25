# Koka's rbtree: 4.2 million keys into a functional red-black tree. A node is
# a tuple (color, left, key, value, right); None is a leaf.

RED, BLACK = 0, 1


def is_red(t):
    return t is not None and t[0] == RED


def balance_left(l, k, v, r):
    if l is None:
        return None
    _, ll, lk, lv, lr = l
    if is_red(ll):
        _, a, xk, xv, b = ll
        return (RED, (BLACK, a, xk, xv, b), lk, lv, (BLACK, lr, k, v, r))
    if is_red(lr):
        _, a, xk, xv, b = lr
        return (RED, (BLACK, ll, lk, lv, a), xk, xv, (BLACK, b, k, v, r))
    return (BLACK, (RED, ll, lk, lv, lr), k, v, r)


def balance_right(l, k, v, r):
    if r is None:
        return None
    _, rl, rk, rv, rr = r
    if is_red(rl):
        _, a, xk, xv, b = rl
        return (RED, (BLACK, l, k, v, a), xk, xv, (BLACK, b, rk, rv, rr))
    if is_red(rr):
        _, a, xk, xv, b = rr
        return (RED, (BLACK, l, k, v, rl), rk, rv, (BLACK, a, xk, xv, b))
    return (BLACK, l, k, v, (RED, rl, rk, rv, rr))


def ins(t, k, v):
    if t is None:
        return (RED, None, k, v, None)
    c, l, kx, vx, r = t
    if c == RED:
        if k < kx:
            return (RED, ins(l, k, v), kx, vx, r)
        if k > kx:
            return (RED, l, kx, vx, ins(r, k, v))
        return (RED, l, k, v, r)
    if k < kx:
        if is_red(l):
            return balance_left(ins(l, k, v), kx, vx, r)
        return (BLACK, ins(l, k, v), kx, vx, r)
    if k > kx:
        if is_red(r):
            return balance_right(l, kx, vx, ins(r, k, v))
        return (BLACK, l, kx, vx, ins(r, k, v))
    return (BLACK, l, k, v, r)


def insert(t, k, v):
    _, l, kx, vx, r = ins(t, k, v)
    return (BLACK, l, kx, vx, r)


def fold(t, b, f):
    while t is not None:
        _, l, k, v, r = t
        b = f(k, v, fold(l, b, f))
        t = r
    return b


def main():
    t = None
    n = 4200000
    while n > 0:
        n -= 1
        t = insert(t, n, n % 10 == 0)
    print(fold(t, 0, lambda k, v, r: r + 1 if v else r))


main()

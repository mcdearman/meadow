"""Threads contending for shared mutable state: eight of them moving money
between sixteen accounts, every transfer reading two accounts and writing two
as one indivisible step. One lock over the whole set.

CPython's own lock means these threads take turns rather than run at once, so
this measures what the locking costs and not what parallelism buys."""

import threading

ACCOUNTS = 16
WORKERS = 8
MOVES = 20000

bank = [1000] * ACCOUNTS
lock = threading.Lock()


def transfers(w):
    s = w + 1
    for _ in range(MOVES):
        s = s * 48271 % 2147483647
        a = s % ACCOUNTS
        b = s // ACCOUNTS % ACCOUNTS
        amount = 1 + s % 10
        if a == b:
            continue
        with lock:
            bank[a] -= amount
            bank[b] += amount


def main():
    threads = [threading.Thread(target=transfers, args=(w,)) for w in range(WORKERS)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    print(",".join(str(b) for b in bank))


if __name__ == "__main__":
    main()

"""Data parallelism: a 2000x2000 grid of independent float work, split between
processes by taking every Nth row.

Processes, not threads: CPython holds a lock on the interpreter, so threads
would take turns. `multiprocessing` is what Python reaches for, and the cost
of starting the processes is part of what it costs."""

import multiprocessing as mp

SIDE = 2000
LIMIT = 100


def band(args):
    start, step = args
    total = 0
    for j in range(start, SIDE, step):
        cy = j / SIDE * 3.0 - 1.5
        for i in range(SIDE):
            cx = i / SIDE * 3.0 - 2.0
            x = y = 0.0
            n = 0
            while n < LIMIT and x * x + y * y <= 4.0:
                x, y = x * x - y * y + cx, 2.0 * x * y + cy
                n += 1
            total += n
    return total


def main():
    workers = mp.cpu_count()
    with mp.Pool(workers) as pool:
        print(sum(pool.map(band, [(w, workers) for w in range(workers)])))


if __name__ == "__main__":
    main()

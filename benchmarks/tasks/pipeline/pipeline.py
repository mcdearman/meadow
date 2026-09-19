"""Message passing: four producer threads each send fifty thousand numbers into
one queue, and the main thread receives all of them and adds them up.

`queue.Queue` is Python's channel. CPython's own lock means the producers take
turns rather than run at once, so this is the cost of the queue and not of
parallelism."""

import queue
import threading

PRODUCERS = 4
EACH = 50_000


def produce(q, p):
    for i in range(EACH):
        q.put(p * EACH + i)


def main():
    q = queue.Queue(maxsize=1024)
    for p in range(PRODUCERS):
        threading.Thread(target=produce, args=(q, p), daemon=True).start()
    total = 0
    for _ in range(PRODUCERS * EACH):
        total += q.get()
    print(total)


if __name__ == "__main__":
    main()

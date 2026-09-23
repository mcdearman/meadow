"""`wordfreq`, with an arena: the same counts, with nothing allocated per word.

The corpus is read once into one `bytes`, and a word is a pair of numbers into
it -- where it starts and how long it is -- so no word is ever copied. The
table is three flat arrays of a fixed size rather than a dict, and probing
writes numbers into them. What is left in the number is the hashing, the
probing and the byte comparisons, with the allocator and the string type taken
out.

Only the ten reported at the end become strings."""

from array import array

CAP = 1 << 14  # 16384 slots for a vocabulary of 5000

starts = array("l", bytes(8 * CAP))
lens = array("l", bytes(8 * CAP))
counts = array("q", bytes(8 * CAP))


def hash_of(text, at, n):
    h = 1469598103934665603
    for i in range(at, at + n):
        h = ((h ^ text[i]) * 1099511628211) & 0xFFFFFFFFFFFFFFFF
    return h


def bump(text, at, n):
    i = hash_of(text, at, n) & (CAP - 1)
    while True:
        if counts[i] == 0:
            starts[i], lens[i], counts[i] = at, n, 1
            return
        if lens[i] == n and text[starts[i] : starts[i] + n] == text[at : at + n]:
            counts[i] += 1
            return
        i = (i + 1) & (CAP - 1)


def main():
    with open("work/corpus.txt", "rb") as f:
        text = f.read()

    size = len(text)
    i = 0
    while i < size:
        while i < size and (text[i] == 32 or text[i] == 10):
            i += 1
        start = i
        while i < size and text[i] != 32 and text[i] != 10:
            i += 1
        if i > start:
            bump(text, start, i - start)

    # The ten commonest: count first, then the bytes.
    filled = [i for i in range(CAP) if counts[i]]
    filled.sort(key=lambda i: (-counts[i], text[starts[i] : starts[i] + lens[i]]))
    print(
        " ".join(
            f"{text[starts[i] : starts[i] + lens[i]].decode()}:{counts[i]}"
            for i in filled[:10]
        )
    )


if __name__ == "__main__":
    main()

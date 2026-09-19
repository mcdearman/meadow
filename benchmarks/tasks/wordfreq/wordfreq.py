"""Strings, a hash map and a sort: count the words of a 3MB file and report the
ten commonest, count first and then the word."""

from collections import Counter


def main():
    with open("work/corpus.txt") as f:
        counts = Counter(f.read().split())
    ranked = sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))
    print(" ".join(f"{w}:{c}" for w, c in ranked[:10]))


if __name__ == "__main__":
    main()

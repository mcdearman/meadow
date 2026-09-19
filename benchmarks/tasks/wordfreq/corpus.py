"""Write the corpus the wordfreq benchmark reads.

Deterministic: the same bytes on every machine, so the checksum is comparable.
A small vocabulary drawn from a skewed distribution, which is what real text
looks like to a hash map -- a few words constantly, a long tail once each.
"""

import sys
from pathlib import Path

WORDS = 400_000
VOCAB = 5_000


def main(out):
    vocab = [f"w{i}x{i * 7 % 97}" for i in range(VOCAB)]
    state = 12345
    parts = []
    for i in range(WORDS):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        # Skewed: squaring a uniform draw pulls most picks towards the front of
        # the vocabulary, so a few words dominate as they do in real text.
        r = (state % VOCAB) * (state // VOCAB % VOCAB) // VOCAB
        parts.append(vocab[r % VOCAB])
        parts.append("\n" if i % 12 == 11 else " ")
    Path(out).write_text("".join(parts) + "\n")


if __name__ == "__main__":
    main(sys.argv[1])

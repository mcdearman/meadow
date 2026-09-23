/* `wordfreq`, with an arena: the same counts, with nothing allocated per word.
 *
 * The corpus is read once into one block, and a word is a pair of numbers into
 * it -- where it starts and how long it is -- so no word is ever copied. The
 * table is three flat arrays of a fixed size rather than a structure per
 * entry, and probing writes numbers into them. What is left in each language's
 * number is the hashing, the probing and the byte comparisons, with its
 * allocator and its string type taken out.
 *
 * Only the ten reported at the end become strings. */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#define CAP (1 << 14) /* 16384 slots for a vocabulary of 5000 */

static int64_t starts[CAP], lens[CAP], counts[CAP];
static char *text;

static uint64_t hash_of(int64_t at, int64_t n) {
    uint64_t h = 1469598103934665603ULL;
    for (int64_t i = 0; i < n; i++) { h ^= (unsigned char)text[at + i]; h *= 1099511628211ULL; }
    return h;
}

static void bump(int64_t at, int64_t n) {
    uint64_t i = hash_of(at, n) & (CAP - 1);
    for (;;) {
        if (counts[i] == 0) { starts[i] = at; lens[i] = n; counts[i] = 1; return; }
        if (lens[i] == n && memcmp(text + starts[i], text + at, (size_t)n) == 0) { counts[i]++; return; }
        i = (i + 1) & (CAP - 1);
    }
}

/* Is the word at `a` before the one at `b`, count first and then the bytes? */
static int before(int64_t a, int64_t b) {
    if (counts[a] != counts[b]) return counts[a] > counts[b];
    int64_t n = lens[a] < lens[b] ? lens[a] : lens[b];
    int c = memcmp(text + starts[a], text + starts[b], (size_t)n);
    if (c) return c < 0;
    return lens[a] < lens[b];
}

int main(void) {
    FILE *f = fopen("work/corpus.txt", "rb");
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    rewind(f);
    text = malloc(size + 1);
    if (fread(text, 1, size, f) != (size_t)size) return 1;
    text[size] = '\0';
    fclose(f);

    for (long i = 0; i < size;) {
        while (i < size && (text[i] == ' ' || text[i] == '\n')) i++;
        long start = i;
        while (i < size && text[i] != ' ' && text[i] != '\n') i++;
        if (i > start) bump(start, i - start);
    }

    /* The ten commonest, kept in order as the table is walked. */
    int64_t top[10];
    int n = 0;
    for (int64_t i = 0; i < CAP; i++) {
        if (counts[i] == 0) continue;
        int at = n;
        while (at > 0 && before(i, top[at - 1])) at--;
        if (at >= 10) continue;
        for (int j = (n < 10 ? n : 9); j > at; j--) top[j] = top[j - 1];
        top[at] = i;
        if (n < 10) n++;
    }
    for (int i = 0; i < n; i++) {
        printf("%.*s:%lld%s", (int)lens[top[i]], text + starts[top[i]],
               (long long)counts[top[i]], i + 1 < n ? " " : "\n");
    }
    return 0;
}

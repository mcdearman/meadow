/* Strings, a hash map and a sort: count the words of a 3MB file and report the
 * ten commonest, count first and then the word.
 *
 * C has no hash map, so this is one: open addressing, linear probing, FNV-1a,
 * grown by doubling. That is the honest comparison -- the C entry of a
 * benchmark like this is always longer than everyone else's, and the reason is
 * worth seeing rather than hiding behind a library. */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

typedef struct { const char *word; size_t len; int64_t count; } Slot;

static Slot *slots;
static size_t cap, used;

static uint64_t hash_of(const char *s, size_t n) {
    uint64_t h = 1469598103934665603ULL;
    for (size_t i = 0; i < n; i++) { h ^= (unsigned char)s[i]; h *= 1099511628211ULL; }
    return h;
}

static void grow(void);

static void bump(const char *s, size_t n) {
    if ((used + 1) * 2 > cap) grow();
    size_t i = hash_of(s, n) & (cap - 1);
    while (slots[i].word) {
        if (slots[i].len == n && memcmp(slots[i].word, s, n) == 0) { slots[i].count++; return; }
        i = (i + 1) & (cap - 1);
    }
    slots[i].word = s; slots[i].len = n; slots[i].count = 1; used++;
}

static void grow(void) {
    size_t was = cap; Slot *old = slots;
    cap = cap ? cap * 2 : 1024;
    slots = calloc(cap, sizeof(Slot));
    for (size_t i = 0; i < was; i++) {
        if (!old[i].word) continue;
        size_t j = hash_of(old[i].word, old[i].len) & (cap - 1);
        while (slots[j].word) j = (j + 1) & (cap - 1);
        slots[j] = old[i];
    }
    free(old);
}

static int by_rank(const void *pa, const void *pb) {
    const Slot *a = pa, *b = pb;
    if (a->count != b->count) return a->count < b->count ? 1 : -1;
    size_t n = a->len < b->len ? a->len : b->len;
    int c = memcmp(a->word, b->word, n);
    if (c) return c;
    return a->len < b->len ? -1 : a->len > b->len;
}

int main(void) {
    FILE *f = fopen("work/corpus.txt", "rb");
    fseek(f, 0, SEEK_END);
    long size = ftell(f);
    rewind(f);
    char *text = malloc(size + 1);
    if (fread(text, 1, size, f) != (size_t)size) return 1;
    text[size] = '\0';
    fclose(f);

    for (long i = 0; i < size;) {
        while (i < size && (text[i] == ' ' || text[i] == '\n')) i++;
        long start = i;
        while (i < size && text[i] != ' ' && text[i] != '\n') i++;
        if (i > start) bump(text + start, (size_t)(i - start));
    }

    Slot *found = malloc(used * sizeof(Slot));
    size_t k = 0;
    for (size_t i = 0; i < cap; i++) if (slots[i].word) found[k++] = slots[i];
    qsort(found, used, sizeof(Slot), by_rank);
    for (int i = 0; i < 10; i++)
        printf("%.*s:%lld%s", (int)found[i].len, found[i].word,
               (long long)found[i].count, i == 9 ? "\n" : " ");
    return 0;
}

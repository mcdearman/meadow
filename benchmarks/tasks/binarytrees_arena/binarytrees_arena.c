/* `binarytrees`, with an arena: the same trees, built into one flat block of
 * nodes and thrown away whole.
 *
 * A node is three slots -- value, left, right -- and a child is the index of
 * one, not a pointer. Building a tree is a bump of a counter per node;
 * discarding one is setting the counter back to nought. Nothing is freed
 * node by node and nothing is collected, which is the point: what is left in
 * each language's number is the writing of the nodes and the walking of them,
 * with its allocator and its collector taken out.
 *
 * The long-lived tree has an arena of its own, since it outlives the rest.
 *
 * Each tip carries the iteration it was built in, so no two trees are alike
 * and no compiler can build one and reuse it. */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

#define TIP (-1)

static int64_t *arena;
static int64_t bump;

/* A node at `i`: arena[i] value, arena[i+1] left, arena[i+2] right. */
static int64_t build(int64_t *a, int64_t *n, int64_t v, int d) {
    int64_t i = *n;
    *n += 3;
    if (d <= 0) {
        a[i] = v;
        a[i + 1] = TIP;
        return i;
    }
    int64_t l = build(a, n, v, d - 1);
    int64_t r = build(a, n, v + 1, d - 1);
    a[i] = 0;
    a[i + 1] = l;
    a[i + 2] = r;
    return i;
}

static int64_t check(const int64_t *a, int64_t i) {
    if (a[i + 1] == TIP) return a[i];
    return check(a, a[i + 1]) + check(a, a[i + 2]);
}

int main(void) {
    const int top = 18;
    /* Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots
     * each. */
    int64_t room = 3 * ((int64_t)1 << (top + 1));
    int64_t *lasting = malloc(room * sizeof(int64_t));
    int64_t lasting_n = 0;
    int64_t root = build(lasting, &lasting_n, 1, top);

    arena = malloc(room * sizeof(int64_t));
    int64_t total = 0;
    for (int d = 4; d <= top; d += 2) {
        for (int64_t i = (int64_t)1 << (top - d + 4); i > 0; i--) {
            bump = 0;
            int64_t t = build(arena, &bump, i, d);
            total += check(arena, t);
        }
    }
    printf("%lld\n", (long long)(total + check(lasting, root)));
    free(arena);
    free(lasting);
    return 0;
}

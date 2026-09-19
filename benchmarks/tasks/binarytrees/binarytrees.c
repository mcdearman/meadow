/* Allocation and collection: build a small tree, walk it, discard it, tens of
 * millions of nodes over, with one long-lived tree kept alive throughout.
 *
 * malloc per node and free per tree -- what C does when it is not reaching for
 * an arena, which is the comparison worth having against a collector.
 *
 * Each tip carries the iteration it was built in, so no two trees are alike
 * and no compiler can build one and reuse it. */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

typedef struct Tree {
    struct Tree *left, *right;
    int64_t value;
} Tree;

static Tree *build(int64_t v, int d) {
    Tree *t = malloc(sizeof(Tree));
    if (d <= 0) {
        t->left = t->right = NULL;
        t->value = v;
    } else {
        t->left = build(v, d - 1);
        t->right = build(v + 1, d - 1);
    }
    return t;
}

static int64_t check(const Tree *t) {
    if (!t->left) return t->value;
    return check(t->left) + check(t->right);
}

static void release(Tree *t) {
    if (t->left) {
        release(t->left);
        release(t->right);
    }
    free(t);
}

int main(void) {
    const int top = 18;
    Tree *lasting = build(1, top);
    int64_t total = 0;
    for (int d = 4; d <= top; d += 2) {
        for (int64_t i = (int64_t)1 << (top - d + 4); i > 0; i--) {
            Tree *t = build(i, d);
            total += check(t);
            release(t);
        }
    }
    printf("%lld\n", (long long)(total + check(lasting)));
    release(lasting);
    return 0;
}

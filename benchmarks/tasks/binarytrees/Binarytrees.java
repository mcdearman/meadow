// Allocation and collection: build a small tree, walk it, discard it, tens of
// millions of nodes over, with one long-lived tree kept alive throughout.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

public class Binarytrees {
    static final class Tree {
        final Tree left, right;
        final long value;

        Tree(long value) {
            this.left = this.right = null;
            this.value = value;
        }

        Tree(Tree left, Tree right) {
            this.left = left;
            this.right = right;
            this.value = 0;
        }
    }

    static Tree build(long v, int d) {
        return d <= 0 ? new Tree(v) : new Tree(build(v, d - 1), build(v + 1, d - 1));
    }

    static long check(Tree t) {
        return t.left == null ? t.value : check(t.left) + check(t.right);
    }

    public static void main(String[] args) {
        final int top = 18;
        Tree lasting = build(1, top);
        long total = 0;
        for (int d = 4; d <= top; d += 2) {
            for (long i = 1L << (top - d + 4); i > 0; i--) {
                total += check(build(i, d));
            }
        }
        System.out.println(total + check(lasting));
    }
}

// `binarytrees`, with an arena: the same trees, built into one flat block of
// nodes and thrown away whole.
//
// A node is three slots -- value, left, right -- and a child is the index of
// one, not an object. Building a tree is a bump of a counter per node;
// discarding one is setting the counter back to nought. What is left in the
// number is writing the nodes and walking them, with the allocator and the
// collector taken out.
//
// Each tip carries the iteration it was built in, so no two trees are alike
// and no compiler can build one and reuse it.

public class Binarytrees_arena {
    static final long TIP = -1;

    static long bump;

    static long build(long[] a, long v, int d) {
        long i = bump;
        bump += 3;
        if (d <= 0) {
            a[(int) i] = v;
            a[(int) i + 1] = TIP;
            return i;
        }
        long l = build(a, v, d - 1);
        long r = build(a, v + 1, d - 1);
        a[(int) i] = 0;
        a[(int) i + 1] = l;
        a[(int) i + 2] = r;
        return i;
    }

    static long check(long[] a, long i) {
        if (a[(int) i + 1] == TIP) return a[(int) i];
        return check(a, a[(int) i + 1]) + check(a, a[(int) i + 2]);
    }

    public static void main(String[] args) {
        final int top = 18;
        // Room for a tree of the deepest kind: 2^(top+1) - 1 nodes, three slots each.
        int room = 3 * (1 << (top + 1));
        long[] lasting = new long[room];
        bump = 0;
        long root = build(lasting, 1, top);

        long[] arena = new long[room];
        long total = 0;
        for (int d = 4; d <= top; d += 2) {
            for (long i = 1L << (top - d + 4); i > 0; i--) {
                bump = 0;
                long t = build(arena, i, d);
                total += check(arena, t);
            }
        }
        System.out.println(total + check(lasting, root));
    }
}

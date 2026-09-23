// `wordfreq`, with an arena: the same counts, with nothing allocated per word.
//
// The corpus is read once into one byte[], and a word is a pair of numbers into
// it -- where it starts and how long it is -- so no word is ever copied. The
// table is three flat arrays of a fixed size rather than a HashMap<String, ?>,
// and probing writes numbers into them. What is left in the number is the
// hashing, the probing and the byte comparisons, with the allocator and the
// string type taken out.
//
// Only the ten reported at the end become strings.

import java.nio.file.Files;
import java.nio.file.Path;

public class Wordfreq_arena {
    static final int CAP = 1 << 14; // 16384 slots for a vocabulary of 5000

    static final int[] starts = new int[CAP];
    static final int[] lens = new int[CAP];
    static final long[] counts = new long[CAP];
    static byte[] text;

    static long hashOf(int at, int n) {
        long h = 1469598103934665603L;
        for (int i = 0; i < n; i++) {
            h ^= (text[at + i] & 0xff);
            h *= 1099511628211L;
        }
        return h;
    }

    static boolean same(int a, int b, int n) {
        for (int i = 0; i < n; i++) if (text[a + i] != text[b + i]) return false;
        return true;
    }

    static void bump(int at, int n) {
        int i = (int) (hashOf(at, n) & (CAP - 1));
        for (;;) {
            if (counts[i] == 0) {
                starts[i] = at; lens[i] = n; counts[i] = 1;
                return;
            }
            if (lens[i] == n && same(starts[i], at, n)) { counts[i]++; return; }
            i = (i + 1) & (CAP - 1);
        }
    }

    // Is the word in slot a before the one in slot b, count first then bytes?
    static boolean before(int a, int b) {
        if (counts[a] != counts[b]) return counts[a] > counts[b];
        int n = Math.min(lens[a], lens[b]);
        for (int i = 0; i < n; i++) {
            int x = text[starts[a] + i] & 0xff, y = text[starts[b] + i] & 0xff;
            if (x != y) return x < y;
        }
        return lens[a] < lens[b];
    }

    public static void main(String[] args) throws Exception {
        text = Files.readAllBytes(Path.of("work/corpus.txt"));

        for (int i = 0; i < text.length;) {
            while (i < text.length && (text[i] == ' ' || text[i] == '\n')) i++;
            int start = i;
            while (i < text.length && text[i] != ' ' && text[i] != '\n') i++;
            if (i > start) bump(start, i - start);
        }

        // The ten commonest, kept in order as the table is walked.
        int[] top = new int[10];
        int n = 0;
        for (int i = 0; i < CAP; i++) {
            if (counts[i] == 0) continue;
            int at = n;
            while (at > 0 && before(i, top[at - 1])) at--;
            if (at >= 10) continue;
            for (int j = Math.min(n, 9); j > at; j--) top[j] = top[j - 1];
            top[at] = i;
            if (n < 10) n++;
        }
        StringBuilder out = new StringBuilder();
        for (int i = 0; i < n; i++) {
            if (i > 0) out.append(' ');
            out.append(new String(text, starts[top[i]], lens[top[i]]))
               .append(':').append(counts[top[i]]);
        }
        System.out.println(out);
    }
}

// Threads contending for shared mutable state: eight of them moving money
// between sixteen accounts, every transfer reading two accounts and writing
// two as one indivisible step. One monitor over the whole set.

import java.util.StringJoiner;

public class Contention {
    static final int ACCOUNTS = 16;
    static final int WORKERS = 8;
    static final int MOVES = 20_000;

    static final long[] bank = new long[ACCOUNTS];
    static final Object lock = new Object();

    public static void main(String[] args) throws InterruptedException {
        java.util.Arrays.fill(bank, 1000);
        Thread[] threads = new Thread[WORKERS];
        for (int w = 0; w < WORKERS; w++) {
            final long seed = w + 1;
            threads[w] = new Thread(() -> {
                long s = seed;
                for (int i = 0; i < MOVES; i++) {
                    s = s * 48271 % 2147483647;
                    int a = (int) (s % ACCOUNTS);
                    int b = (int) (s / ACCOUNTS % ACCOUNTS);
                    long amount = 1 + s % 10;
                    if (a == b) continue;
                    synchronized (lock) {
                        bank[a] -= amount;
                        bank[b] += amount;
                    }
                }
            });
            threads[w].start();
        }
        for (Thread t : threads) t.join();
        StringJoiner out = new StringJoiner(",");
        for (long b : bank) out.add(Long.toString(b));
        System.out.println(out);
    }
}

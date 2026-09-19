// Message passing: four producer threads each send fifty thousand numbers into
// one queue, and the main thread receives all of them and adds them up.

import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.BlockingQueue;

public class Pipeline {
    static final int PRODUCERS = 4;
    static final int EACH = 50_000;

    public static void main(String[] args) throws InterruptedException {
        BlockingQueue<Long> q = new ArrayBlockingQueue<>(1024);
        for (int p = 0; p < PRODUCERS; p++) {
            final long base = (long) p * EACH;
            Thread t = new Thread(() -> {
                try {
                    for (int i = 0; i < EACH; i++) q.put(base + i);
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
            });
            t.setDaemon(true);
            t.start();
        }
        long total = 0;
        for (int n = 0; n < PRODUCERS * EACH; n++) total += q.take();
        System.out.println(total);
    }
}

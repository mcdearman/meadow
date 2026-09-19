// Data parallelism: a 2000x2000 grid of independent float work, split between
// threads by taking every Nth row.

import java.util.stream.IntStream;

public class Mandelbrot {
    static final int SIDE = 2000;
    static final int LIMIT = 100;

    static long band(int start, int step) {
        long total = 0;
        for (int j = start; j < SIDE; j += step) {
            double cy = (double) j / SIDE * 3.0 - 1.5;
            for (int i = 0; i < SIDE; i++) {
                double cx = (double) i / SIDE * 3.0 - 2.0;
                double x = 0.0, y = 0.0;
                long n = 0;
                while (n < LIMIT && x * x + y * y <= 4.0) {
                    double nx = x * x - y * y + cx;
                    y = 2.0 * x * y + cy;
                    x = nx;
                    n++;
                }
                total += n;
            }
        }
        return total;
    }

    public static void main(String[] args) {
        int workers = Runtime.getRuntime().availableProcessors();
        System.out.println(
                IntStream.range(0, workers).parallel().mapToLong(w -> band(w, workers)).sum());
    }
}

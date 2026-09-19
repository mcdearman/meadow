// Three nested loops over flat arrays of doubles: the shape of numerical code,
// and of nothing else. `i k j` order, so the innermost loop walks in order.

public class Matmul {
    static final int N = 256;

    public static void main(String[] args) {
        double[] a = new double[N * N];
        double[] b = new double[N * N];
        double[] c = new double[N * N];
        for (int i = 0; i < N; i++) {
            for (int j = 0; j < N; j++) {
                a[i * N + j] = (i + j) % 10;
                b[i * N + j] = (i * j) % 10;
            }
        }
        for (int i = 0; i < N; i++) {
            for (int k = 0; k < N; k++) {
                double aik = a[i * N + k];
                for (int j = 0; j < N; j++) {
                    c[i * N + j] += aik * b[k * N + j];
                }
            }
        }
        double total = 0;
        for (int i = 0; i < N; i++) total += c[i * N + i];
        System.out.println((long) total);
    }
}

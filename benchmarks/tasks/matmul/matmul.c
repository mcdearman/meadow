/* Three nested loops over flat arrays of doubles: the shape of numerical code,
 * and of nothing else. `i k j` order, so the innermost loop walks in order. */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>

#define N 256

int main(void) {
    double *a = calloc(N * N, sizeof(double));
    double *b = calloc(N * N, sizeof(double));
    double *c = calloc(N * N, sizeof(double));
    for (int i = 0; i < N; i++) {
        for (int j = 0; j < N; j++) {
            a[i * N + j] = (double)((i + j) % 10);
            b[i * N + j] = (double)((i * j) % 10);
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
    printf("%lld\n", (long long)total);
    free(a); free(b); free(c);
    return 0;
}

/* Data parallelism: a 2000x2000 grid of independent float work, split between
 * threads by taking every Nth row. */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <pthread.h>
#include <unistd.h>

#define SIDE 2000
#define LIMIT 100

typedef struct { int64_t start, step, total; } Band;

static void *band(void *arg) {
    Band *b = arg;
    int64_t total = 0;
    for (int64_t j = b->start; j < SIDE; j += b->step) {
        double cy = (double)j / (double)SIDE * 3.0 - 1.5;
        for (int64_t i = 0; i < SIDE; i++) {
            double cx = (double)i / (double)SIDE * 3.0 - 2.0;
            double x = 0.0, y = 0.0;
            int64_t n = 0;
            while (n < LIMIT && x * x + y * y <= 4.0) {
                double nx = x * x - y * y + cx;
                y = 2.0 * x * y + cy;
                x = nx;
                n++;
            }
            total += n;
        }
    }
    b->total = total;
    return NULL;
}

int main(void) {
    long workers = sysconf(_SC_NPROCESSORS_ONLN);
    if (workers < 1) workers = 4;
    pthread_t *ts = malloc(workers * sizeof(pthread_t));
    Band *bands = malloc(workers * sizeof(Band));
    for (long w = 0; w < workers; w++) {
        bands[w] = (Band){w, workers, 0};
        pthread_create(&ts[w], NULL, band, &bands[w]);
    }
    int64_t total = 0;
    for (long w = 0; w < workers; w++) {
        pthread_join(ts[w], NULL);
        total += bands[w].total;
    }
    printf("%lld\n", (long long)total);
    free(ts); free(bands);
    return 0;
}

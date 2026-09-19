/* Message passing: four producer threads each send fifty thousand numbers into
 * one queue, and the main thread receives all of them and adds them up.
 *
 * C has no channel, so this is one: a fixed ring buffer, a mutex and two
 * condition variables. Every other language in this suite has this in its
 * standard library, and the length of this file against theirs is the point. */

#include <stdio.h>
#include <stdint.h>
#include <pthread.h>

#define PRODUCERS 4
#define EACH 50000
#define CAP 1024

static int64_t ring[CAP];
static size_t head, tail, count;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t not_full = PTHREAD_COND_INITIALIZER;
static pthread_cond_t not_empty = PTHREAD_COND_INITIALIZER;

static void put(int64_t v) {
    pthread_mutex_lock(&lock);
    while (count == CAP) pthread_cond_wait(&not_full, &lock);
    ring[tail] = v;
    tail = (tail + 1) % CAP;
    count++;
    pthread_cond_signal(&not_empty);
    pthread_mutex_unlock(&lock);
}

static int64_t take(void) {
    pthread_mutex_lock(&lock);
    while (count == 0) pthread_cond_wait(&not_empty, &lock);
    int64_t v = ring[head];
    head = (head + 1) % CAP;
    count--;
    pthread_cond_signal(&not_full);
    pthread_mutex_unlock(&lock);
    return v;
}

static void *produce(void *arg) {
    int64_t p = (int64_t)(intptr_t)arg;
    for (int64_t i = 0; i < EACH; i++) put(p * EACH + i);
    return NULL;
}

int main(void) {
    pthread_t ts[PRODUCERS];
    for (intptr_t p = 0; p < PRODUCERS; p++) pthread_create(&ts[p], NULL, produce, (void *)p);
    int64_t total = 0;
    for (int64_t n = 0; n < PRODUCERS * EACH; n++) total += take();
    for (int p = 0; p < PRODUCERS; p++) pthread_join(ts[p], NULL);
    printf("%lld\n", (long long)total);
    return 0;
}

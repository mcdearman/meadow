/* Threads contending for shared mutable state: eight of them moving money
 * between sixteen accounts, every transfer reading two accounts and writing
 * two as one indivisible step. One mutex over the whole set. */

#include <stdio.h>
#include <stdint.h>
#include <pthread.h>

#define ACCOUNTS 16
#define WORKERS 8
#define MOVES 20000

static int64_t bank[ACCOUNTS];
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;

static void *transfers(void *arg) {
    int64_t s = (int64_t)(intptr_t)arg + 1;
    for (int64_t i = 0; i < MOVES; i++) {
        s = s * 48271 % 2147483647;
        int a = (int)(s % ACCOUNTS);
        int b = (int)(s / ACCOUNTS % ACCOUNTS);
        int64_t amount = 1 + s % 10;
        if (a == b) continue;
        pthread_mutex_lock(&lock);
        bank[a] -= amount;
        bank[b] += amount;
        pthread_mutex_unlock(&lock);
    }
    return NULL;
}

int main(void) {
    for (int i = 0; i < ACCOUNTS; i++) bank[i] = 1000;
    pthread_t ts[WORKERS];
    for (intptr_t w = 0; w < WORKERS; w++) pthread_create(&ts[w], NULL, transfers, (void *)w);
    for (int w = 0; w < WORKERS; w++) pthread_join(ts[w], NULL);
    for (int i = 0; i < ACCOUNTS; i++)
        printf("%lld%s", (long long)bank[i], i == ACCOUNTS - 1 ? "\n" : ",");
    return 0;
}

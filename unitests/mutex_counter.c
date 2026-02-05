#define _POSIX_C_SOURCE 200809L

#include <assert.h>
#include <errno.h>
#include <inttypes.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t counter_mu = PTHREAD_MUTEX_INITIALIZER;
static uint64_t counter = 0;
static pthread_barrier_t start_barrier;
static volatile uint64_t work_sink = 0;

struct worker_args {
  uint64_t loops;
  uint64_t hold_iters;
};

static void *worker_main(void *arg) {
  const struct worker_args *args = (const struct worker_args *)arg;
  int brc = pthread_barrier_wait(&start_barrier);
  if (brc != 0 && brc != PTHREAD_BARRIER_SERIAL_THREAD) {
    errno = brc;
    perror("pthread_barrier_wait");
    abort();
  }
  for (uint64_t i = 0; i < args->loops; i++) {
    int rc = pthread_mutex_lock(&counter_mu);
    if (rc != 0) {
      errno = rc;
      perror("pthread_mutex_lock");
      abort();
    }
    for (uint64_t j = 0; j < args->hold_iters; j++) {
      work_sink += (j ^ i);
    }
    counter++;
    rc = pthread_mutex_unlock(&counter_mu);
    if (rc != 0) {
      errno = rc;
      perror("pthread_mutex_unlock");
      abort();
    }
  }
  return NULL;
}

static uint64_t parse_u64_or_die(const char *s, const char *name) {
  errno = 0;
  char *end = NULL;
  unsigned long long v = strtoull(s, &end, 10);
  if (errno != 0 || end == s || *end != '\0') {
    fprintf(stderr, "invalid %s: '%s'\n", name, s);
    exit(2);
  }
  return (uint64_t)v;
}

static uint64_t timespec_diff_ns(struct timespec end, struct timespec start) {
  int64_t sec = (int64_t)end.tv_sec - (int64_t)start.tv_sec;
  int64_t nsec = (int64_t)end.tv_nsec - (int64_t)start.tv_nsec;
  if (nsec < 0) {
    sec--;
    nsec += 1000000000LL;
  }
  if (sec < 0) {
    return 0;
  }
  return (uint64_t)sec * 1000000000ULL + (uint64_t)nsec;
}

int main(int argc, char **argv) {
  if (argc != 3 && argc != 4) {
    fprintf(stderr, "usage: %s <threads_t> <loops_n> [hold_iters]\n", argv[0]);
    return 2;
  }

  const uint64_t threads = parse_u64_or_die(argv[1], "threads_t");
  const uint64_t loops = parse_u64_or_die(argv[2], "loops_n");
  const uint64_t hold_iters =
      (argc == 4) ? parse_u64_or_die(argv[3], "hold_iters") : 0;
  if (threads == 0 || loops == 0) {
    fprintf(stderr, "threads_t and loops_n must be > 0\n");
    return 2;
  }

  pthread_t *tids = calloc((size_t)threads, sizeof(*tids));
  if (!tids) {
    perror("calloc tids");
    return 1;
  }

  struct worker_args args = {.loops = loops, .hold_iters = hold_iters};

  int rc = pthread_barrier_init(&start_barrier, NULL, (unsigned)threads + 1U);
  if (rc != 0) {
    errno = rc;
    perror("pthread_barrier_init");
    return 1;
  }

  for (uint64_t i = 0; i < threads; i++) {
    rc = pthread_create(&tids[i], NULL, worker_main, &args);
    if (rc != 0) {
      errno = rc;
      perror("pthread_create");
      return 1;
    }
  }

  struct timespec t0;
  struct timespec t1;
  if (clock_gettime(CLOCK_MONOTONIC, &t0) != 0) {
    perror("clock_gettime t0");
    return 1;
  }
  rc = pthread_barrier_wait(&start_barrier);
  if (rc != 0 && rc != PTHREAD_BARRIER_SERIAL_THREAD) {
    errno = rc;
    perror("pthread_barrier_wait (main)");
    return 1;
  }

  for (uint64_t i = 0; i < threads; i++) {
    rc = pthread_join(tids[i], NULL);
    if (rc != 0) {
      errno = rc;
      perror("pthread_join");
      return 1;
    }
  }

  if (clock_gettime(CLOCK_MONOTONIC, &t1) != 0) {
    perror("clock_gettime t1");
    return 1;
  }

  const uint64_t expected = threads * loops;
  assert(counter == expected);
  printf("OK: counter=%" PRIu64 " expected=%" PRIu64 " work_sink=%" PRIu64 "\n",
         counter, expected, (uint64_t)work_sink);

  const uint64_t elapsed_ns = timespec_diff_ns(t1, t0);
  const long double elapsed_s = (long double)elapsed_ns / 1000000000.0L;
  const uint64_t ops = expected;
  const long double qps =
      (elapsed_ns == 0)
          ? 0.0L
          : ((long double)ops * 1000000000.0L) / (long double)elapsed_ns;
  const long double avg_op_ns =
      (ops == 0 || elapsed_ns == 0)
          ? 0.0L
          : (long double)elapsed_ns / (long double)ops;
  const long ncpu = sysconf(_SC_NPROCESSORS_ONLN);
  printf("STATS: threads=%" PRIu64 " loops=%" PRIu64 " hold_iters=%" PRIu64
         " cpu=%ld ops=%" PRIu64
         " elapsed_s=%.6Lf avg_op_ns=%.2Lf rate=%.2Lf Mops/s\n",
         threads, loops, hold_iters, ncpu, ops, elapsed_s, avg_op_ns, qps / 1e6L);

  rc = pthread_barrier_destroy(&start_barrier);
  if (rc != 0) {
    errno = rc;
    perror("pthread_barrier_destroy");
    return 1;
  }
  free(tids);
  return 0;
}

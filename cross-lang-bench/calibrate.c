/*
 * calibrate.c Measure ns_per_tick for the platform counter.
 *
 * Reads the hardware counter (RDTSC on x86_64, CNTVCT_EL0 on aarch64)
 * against CLOCK_MONOTONIC over 5 trials of 10 ms each. Prints the
 * minimum ns_per_tick to stdout.
 *
 * Build:
 *   cc -O2 -o calibrate calibrate.c
 *
 * The minimum is correct because scheduling jitter only ever inflates
 * the wall-clock delta between the two reads; the trial with the
 * smallest ns_per_tick has the least jitter.
 */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <time.h>

/* -- Platform counter ------------------------------------------------ */

#if defined(__x86_64__) || defined(_M_X64)
static inline uint64_t read_counter(void) {
    unsigned int lo, hi;
    /* Read the 64-bit Time Stamp Counter into EDX:EAX.
     * No serialization -- this is calibration, not a timed region. */
    __asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | lo;
}
#elif defined(__aarch64__)
static inline uint64_t read_counter(void) {
    uint64_t val;
    /* Read the ARM Generic Timer virtual counter. Available at EL0 on
     * every ARMv8-A implementation. No side effects. */
    __asm__ volatile("mrs %0, cntvct_el0" : "=r"(val));
    return val;
}
#else
#  error "unsupported architecture -- expected x86_64 or aarch64"
#endif

/* -- Wall clock via CLOCK_MONOTONIC ----------------------------------- */

static int64_t wall_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

/* -- Spin for exactly `ns` nanoseconds -------------------------------- */

static void spin_ns(int64_t ns) {
    int64_t start = wall_ns();
    while (wall_ns() - start < ns) {
        /* busy-wait -- calibration, not production */
    }
}

/* -- One trial -------------------------------------------------------- */

struct trial {
    double ns_per_tick;
};

/*
 * Spin for SPIN_DURATION_NS, measuring counter ticks elapsed against
 * wall-clock nanoseconds. Returns ns_per_tick.
 */
static struct trial run_trial(void) {
    /* Warm the counter -- first read can be slow on some CPUs. */
    (void)read_counter();

    int64_t wall_start = wall_ns();
    uint64_t tsc_start  = read_counter();

    spin_ns(10000000LL); /* 10 ms */

    uint64_t tsc_end  = read_counter();
    int64_t wall_end   = wall_ns();

    int64_t wall_delta = wall_end - wall_start;
    uint64_t tsc_delta = tsc_end - tsc_start;

    struct trial t;
    t.ns_per_tick = (double)wall_delta / (double)tsc_delta;
    return t;
}

/* -- Main ------------------------------------------------------------- */

#define N_TRIALS 5

int main(void) {
    double best = 1e9; /* large sentinel */

    for (int i = 0; i < N_TRIALS; i++) {
        struct trial t = run_trial();
        if (t.ns_per_tick < best) {
            best = t.ns_per_tick;
        }
    }

    /*
     * Print with enough precision for sub-nanosecond per-tick values.
     * At 3 GHz, ns_per_tick ~ 0.333. 6 decimal places covers ~1 ps
     * resolution -- far finer than the calibration accuracy.
     */
    printf("%.6f\n", best);
    return 0;
}

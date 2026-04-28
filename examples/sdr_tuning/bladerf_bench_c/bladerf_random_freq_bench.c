/*
 * bladerf_random_freq_bench.c — Random frequency change benchmark
 *
 * Tests two methods with N random frequency hops in bladeRF 2.0 range:
 *   1. bladerf_set_frequency()      — full PLL recalculation
 *   2. bladerf_schedule_retune()    — pre-computed quick_tune
 *
 * Output: bladerf_random_freq_bench.csv
 *   method,iteration,from_hz,to_hz,time_us
 *
 * Build:  make bladerf_random_freq_bench
 * Run:    ./bladerf_random_freq_bench [iterations]
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <time.h>
#include <libbladeRF.h>

#define FREQ_MIN   70000000ULL     /*   70 MHz */
#define FREQ_MAX   5900000000ULL   /* 5900 MHz */
#define NUM_FREQS  200
#define DEFAULT_N  1000

static double now_us(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e6 + ts.tv_nsec / 1e3;
}

static uint64_t rand_freq(void)
{
    uint64_t range = (FREQ_MAX - FREQ_MIN) / 1000;
    uint64_t r = ((uint64_t)rand() << 32) | (uint64_t)rand();
    return FREQ_MIN + (r % range) * 1000;
}

#define CHECK(call) do {                                       \
    int _s = (call);                                           \
    if (_s != 0) {                                             \
        fprintf(stderr, "ERROR: %s → %s\n", #call,            \
                bladerf_strerror(_s));                          \
        goto cleanup;                                          \
    }                                                          \
} while (0)

int main(int argc, char **argv)
{
    int n = (argc > 1) ? atoi(argv[1]) : DEFAULT_N;
    if (n < 1) n = DEFAULT_N;

    struct bladerf *dev = NULL;
    FILE *csv = NULL;
    uint64_t freqs[NUM_FREQS];
    struct bladerf_quick_tune qt[NUM_FREQS];
    int *indices = NULL;

    printf("=== bladeRF Random Frequency Benchmark ===\n");
    printf("Range: %llu – %llu MHz, %d iterations per method\n",
           (unsigned long long)(FREQ_MIN / 1000000),
           (unsigned long long)(FREQ_MAX / 1000000), n);

    srand((unsigned)time(NULL));
    for (int i = 0; i < NUM_FREQS; i++)
        freqs[i] = rand_freq();

    /* Pre-generate random index sequence */
    indices = malloc(n * sizeof(int));
    for (int i = 0; i < n; i++)
        indices[i] = rand() % NUM_FREQS;

    /* Open device */
    int status = bladerf_open(&dev, NULL);
    if (status != 0) {
        fprintf(stderr, "Failed to open bladeRF: %s\n", bladerf_strerror(status));
        return 1;
    }
    printf("Board: %s\n\n", bladerf_get_board_name(dev));

    bladerf_channel ch = BLADERF_CHANNEL_RX(0);
    CHECK(bladerf_set_sample_rate(dev, ch, 4000000, NULL));
    CHECK(bladerf_set_gain(dev, ch, 40));

    /* Pre-compute quick_tune for all frequencies */
    printf("Pre-computing quick_tune for %d frequencies...\n", NUM_FREQS);
    for (int i = 0; i < NUM_FREQS; i++) {
        CHECK(bladerf_set_frequency(dev, ch, freqs[i]));
        CHECK(bladerf_get_quick_tune(dev, ch, &qt[i]));
    }
    printf("Done.\n\n");

    csv = fopen("bladerf_random_freq_bench.csv", "w");
    if (!csv) { perror("fopen"); goto cleanup; }
    fprintf(csv, "method,iteration,from_hz,to_hz,time_us\n");

    /* Method 1: set_frequency */
    {
        printf("Running set_frequency (%d iterations)...\n", n);
        int prev = 0;
        CHECK(bladerf_set_frequency(dev, ch, freqs[0]));
        double sum = 0;
        for (int i = 0; i < n; i++) {
            int next = indices[i];
            double t = now_us();
            CHECK(bladerf_set_frequency(dev, ch, freqs[next]));
            double elapsed = now_us() - t;
            fprintf(csv, "set_frequency,%d,%llu,%llu,%.1f\n",
                    i, (unsigned long long)freqs[prev],
                    (unsigned long long)freqs[next], elapsed);
            sum += elapsed;
            prev = next;
        }
        printf("  avg=%.0f µs\n\n", sum / n);
    }

    /* Method 2: schedule_retune */
    {
        printf("Running schedule_retune (%d iterations)...\n", n);
        int prev = 0;
        CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0, &qt[0]));
        double sum = 0;
        for (int i = 0; i < n; i++) {
            int next = indices[i];
            double t = now_us();
            CHECK(bladerf_schedule_retune(dev, ch, BLADERF_RETUNE_NOW, 0, &qt[next]));
            double elapsed = now_us() - t;
            fprintf(csv, "schedule_retune,%d,%llu,%llu,%.1f\n",
                    i, (unsigned long long)freqs[prev],
                    (unsigned long long)freqs[next], elapsed);
            sum += elapsed;
            prev = next;
        }
        printf("  avg=%.0f µs\n\n", sum / n);
    }

    fclose(csv); csv = NULL;
    printf("CSV written to bladerf_random_freq_bench.csv\n");

cleanup:
    free(indices);
    if (csv) fclose(csv);
    if (dev) bladerf_close(dev);
    return 0;
}
